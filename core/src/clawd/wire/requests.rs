//! Typed request bodies — one per broker route.
//!
//! These are the broker boundary. A route's parameters stop being
//! `serde_json::Value` here: every struct is `deny_unknown_fields`, so
//! a field the route never declared is a decode failure *before*
//! authorization, and every field is one of the bounded types in
//! [`super::bounded`], so a value that is too long, too deep or the
//! wrong JSON type is refused before any handler sees it.
//!
//! A handful of fields stay [`Structured`] because their shape is the
//! route's public contract and the owning authority — not the broker —
//! is what validates them: a canonical [`crate::caps::Scope`], a
//! serialized [`crate::caps::CapSet`], an App session tool call, a
//! scheduler argument vector, a context source document. Those are
//! still size-bounded here, and the containing request is still typed
//! and closed.
//!
//! After decoding, the registry re-serializes the struct back into the
//! canonical object the handler reads. Nothing survives that round trip
//! except fields this module declared, so a handler cannot reach a
//! value the boundary did not validate.

use serde::{Deserialize, Serialize};

use super::bounded::{Name, NoParams, Structured, Text, TextList, Token, WaitMillis};

/// Free-text ceilings. A request frame is capped well below the sum of
/// these, so they bound one field rather than the message.
const PROMPT_BYTES: usize = 512 * 1024;
const PATH_BYTES: usize = 4096;
const LABEL_BYTES: usize = 1024;
const COMMAND_BYTES: usize = 8192;
const NOTIFICATION_BODY_BYTES: usize = 16 * 1024;
const BROWSER_VALUE_BYTES: usize = 64 * 1024;

pub type NoBody = NoParams;

pub use crate::clawd::regional_settings::Request as RegionalSettingsControl;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppPermissions {
    pub action: Name,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Text<4096>>,
}

// ---------------------------------------------------------------------------
// Activities
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityResource {
    pub label: Text<240>,
    pub reference: Text<4096>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct ActivityResources(Vec<ActivityResource>);

impl<'de> Deserialize<'de> for ActivityResources {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let resources = Vec::<ActivityResource>::deserialize(deserializer)?;
        if resources.len() > 32 {
            return Err(serde::de::Error::custom(
                "activity resource list exceeds its maximum length",
            ));
        }
        Ok(Self(resources))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityCreate {
    pub title: Text<240>,
    pub goal: Text<16384>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_criteria: Option<Text<8192>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundaries: Option<Text<8192>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ActivityResources>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityList {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<crate::activities::ActivityState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityGet {
    pub id: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityUpdate {
    pub id: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Text<240>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<Text<16384>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_criteria: Option<Text<8192>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundaries: Option<Text<8192>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ActivityResources>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityTransition {
    pub id: Token,
    pub state: crate::activities::ActivityState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_note: Option<Text<8192>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityRun {
    pub id: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<Text<PROMPT_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_memory: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Text<4096>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityContinuityExport {
    pub id: Token,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityContinuityImport {
    pub placement: crate::activities::ActivityExecutionPlacement,
    pub document: Text<{ crate::activities::MAX_CONTINUITY_DOCUMENT_BYTES }>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct BoundedObjectRef(pub crate::objects::ObjectRef);

impl<'de> Deserialize<'de> for BoundedObjectRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        claw_os_sdk::generated::validate_object_ref(&value)
            .map_err(|_| serde::de::Error::custom("invalid App object reference fields"))?;
        let object: crate::objects::ObjectRef = serde_json::from_value(value)
            .map_err(|_| serde::de::Error::custom("invalid App object reference types"))?;
        crate::objects::validate(&object)
            .map_err(|_| serde::de::Error::custom("invalid or oversized App object reference"))?;
        Ok(Self(object))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityObjects {
    pub id: Token,
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct BoundedExecutionLimits(pub crate::activities::ExecutionLimitsDraft);

impl<'de> Deserialize<'de> for BoundedExecutionLimits {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let limits = crate::activities::ExecutionLimitsDraft::deserialize(deserializer)?;
        limits
            .validate()
            .map_err(|_| serde::de::Error::custom("invalid Activity execution limits"))?;
        Ok(Self(limits))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityExecutionLimitsGet {
    pub id: Token,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityExecutionLimitsSet {
    pub id: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    pub limits: BoundedExecutionLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityExecutionLimitsEnabled {
    pub id: Token,
    pub expected_revision: u64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct BoundedMonetaryBudget(pub crate::activities::MonetaryBudgetDraft);

impl<'de> Deserialize<'de> for BoundedMonetaryBudget {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let budget = crate::activities::MonetaryBudgetDraft::deserialize(deserializer)?;
        budget
            .validate()
            .map_err(|_| serde::de::Error::custom("invalid Activity monetary budget"))?;
        Ok(Self(budget))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityMonetaryBudgetGet {
    pub id: Token,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityMonetaryBudgetSet {
    pub id: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    pub budget: BoundedMonetaryBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityMonetaryBudgetEnabled {
    pub id: Token,
    pub expected_revision: u64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityObjectAttach {
    pub id: Token,
    pub label: Text<240>,
    pub object: BoundedObjectRef,
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct BoundedCapabilityPolicy(pub crate::activities::CapabilityPolicyDraft);

impl<'de> Deserialize<'de> for BoundedCapabilityPolicy {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let policy = crate::activities::CapabilityPolicyDraft::deserialize(deserializer)?;
        policy
            .validate()
            .map_err(|_| serde::de::Error::custom("invalid Activity capability policy"))?;
        Ok(Self(policy))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityCapabilityPolicyGet {
    pub id: Token,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityCapabilityPolicySet {
    pub id: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    pub policy: BoundedCapabilityPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityCapabilityPolicyEnabled {
    pub id: Token,
    pub expected_revision: u64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivitySchedulingPolicyGet {
    pub id: Token,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivitySchedulingPolicySet {
    pub id: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    pub priority: crate::activities::ActivitySchedulingPriority,
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct BoundedObjectStateDraft(pub crate::activities::ObjectStateDraft);

impl<'de> Deserialize<'de> for BoundedObjectStateDraft {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let draft = crate::activities::ObjectStateDraft::deserialize(deserializer)?;
        draft
            .validate()
            .map_err(|_| serde::de::Error::custom("invalid or oversized object-state entry"))?;
        Ok(Self(draft))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityObjectState {
    pub id: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Text<4096>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityObjectStateRecord {
    pub id: Token,
    pub entry: BoundedObjectStateDraft,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationPreview {
    pub app_id: Name<128>,
    pub operation: Token<128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<TextList<64, 8192>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityOperationPreview {
    pub id: Token,
    pub app_id: Name<128>,
    pub operation: Token<128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<TextList<64, 8192>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct BoundedReceiptReport(pub crate::activities::ReceiptReport);

impl<'de> Deserialize<'de> for BoundedReceiptReport {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let report = crate::activities::ReceiptReport::deserialize(deserializer)?;
        report
            .validate()
            .map_err(|_| serde::de::Error::custom("invalid or oversized receipt report"))?;
        Ok(Self(report))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityReceipts {
    pub id: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityReceiptRecord {
    pub id: Token,
    pub report: BoundedReceiptReport,
}

// ---------------------------------------------------------------------------
// Agent conversations and tasks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConversationCreate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Text<512>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConversationGet {
    pub id: Token<128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConversationList {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConversationUpdate {
    pub id: Token<128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Text<512>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConversationFork {
    pub id: Token<128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_user_turn: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConversationRevert {
    pub id: Token<128>,
    pub user_turns: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSubmit {
    pub prompt: Text<PROMPT_BYTES>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Text<4096>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Text<PROMPT_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_context: Option<Text<PROMPT_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Text<256>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_memory: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskWorkspaceResolve {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<Text<4096>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskList {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskId {
    pub id: Token,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskWait {
    pub id: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<WaitMillis>,
}

// ---------------------------------------------------------------------------
// Memory, context and journals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryHistory {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySessions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentUsage {
    pub args: TextList<16, 256>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemOperations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Token>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextUpdate {
    pub source: Token,
    /// The collector document itself. Shape belongs to the source, not
    /// to the broker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Structured>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Structured>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextEventAppend {
    pub event_type: Token,
    pub source: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<Token>,
    /// Event body. Producers own the schema; the broker only bounds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Structured>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Structured>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextEventQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationPublish {
    pub source: Name<128>,
    pub kind: Name<128>,
    pub severity: Token<32>,
    pub title: Text<LABEL_BYTES>,
    pub body: Text<NOTIFICATION_BODY_BYTES>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_policy: Option<Token<32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<Name<192>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<Token<192>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Token<192>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<Token<192>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Structured>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationList {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_dismissed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationSubscribe {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<WaitMillis>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationId {
    pub id: Token<192>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationPreferencesSet {
    pub web_enabled: bool,
    pub desktop_enabled: bool,
    pub ntfy_enabled: bool,
    pub web_min_severity: Token<32>,
    pub desktop_min_severity: Token<32>,
    pub ntfy_min_severity: Token<32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted_kinds: Option<TextList<128, 128>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dnd_start_minute_utc: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dnd_end_minute_utc: Option<u16>,
    pub critical_bypasses_dnd: bool,
    pub retention_days: u16,
    pub ntfy_server: Text<2048>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ntfy_topic: Option<Name<192>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationDeliveryClaim {
    pub channel: Token<32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_ms: Option<WaitMillis>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationDeliveryComplete {
    pub id: Token<192>,
    pub channel: Token<32>,
    pub status: Token<32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<Token<128>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppNotificationControl {
    pub session: Token,
    pub deadline_unix_ms: u64,
    pub request: AppNotificationRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppNotificationRequest {
    Post {
        summary: Text<960>,
        body: Text<16_000>,
        app_name: Text<512>,
        icon: Text<128>,
        expire_ms: i32,
        transient: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dedupe_key: Option<Name<128>>,
    },
    Close {
        id: Token<64>,
    },
    Send {
        message: Text<16_000>,
        urgent: bool,
    },
    List {
        limit: u32,
    },
}

// ---------------------------------------------------------------------------
// Transactions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionBegin {
    pub purpose: Text<LABEL_BYTES>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionId {
    pub id: Token,
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionList {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionStatus {
    pub ids: TextList<64, 128>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionRequest {
    pub verb: Name,
    /// A canonical [`crate::caps::Scope`]; `permissions` parses it into
    /// the typed scope model before anything is filed.
    pub scope: Structured,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Token>,
    pub reason: Text<LABEL_BYTES>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionDecide {
    pub id: Token,
    pub decision: Token,
    /// Honoured only for a root peer — the privileged approval helper
    /// naming the desktop user it authenticated. A non-root peer is
    /// refused by the route before this is read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<Text<LABEL_BYTES>>,
}

/// Retire reusable approvals for an owner or one of their grant
/// sessions.
///
/// Root-only at the route's access class, because `owner_uid` names
/// whose authority is being retired and a non-root peer must not be
/// able to choose another account — in either direction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionRevoke {
    /// The account whose approvals are retired. Absent means the
    /// unattributed, system-scoped ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_uid: Option<u32>,
    /// One grant session. Absent retires everything the owner holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Token>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemReviewPrepare {
    pub source: Text<PATH_BYTES>,
    pub expected_package: Structured,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemReviewId {
    pub id: Token,
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct SystemReviewDecision(pub clawd_client::system_review::ReviewDecision);

impl<'de> Deserialize<'de> for SystemReviewDecision {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Structured::deserialize(deserializer)?;
        let bytes = serde_json::to_vec(value.as_value()).map_err(serde::de::Error::custom)?;
        clawd_client::system_review::ReviewDecision::decode(&bytes)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedSystemReviewDecide {
    pub owner_uid: u32,
    pub review: SystemReviewDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacySystemReviewDecide {
    pub id: Token,
    pub owner_uid: u32,
    pub decision: Name,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemReviewDecide {
    Typed(TypedSystemReviewDecide),
    Legacy(LegacySystemReviewDecide),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedSystemReviewCancel {
    pub review: SystemReviewDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemReviewCancel {
    Typed(TypedSystemReviewCancel),
    Legacy(SystemReviewId),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemReviewConsume {
    pub id: Token,
    pub source: Text<PATH_BYTES>,
    pub expected_package: Structured,
}

// ---------------------------------------------------------------------------
// App / MCP sessions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppServiceCall {
    pub app_id: Name,
    pub tool: Text<LABEL_BYTES>,
    pub arguments: Structured,
    pub audit: crate::extension_host::protocol::AppInvocationAudit,
}

/// An authenticated local CLI App invocation.
///
/// Deliberately carries only the exact target the human named — App id,
/// tool, and validated arguments. It has no audit binding, call context,
/// caller identity, capability set, package identity, owner uid, or
/// deadline: the daemon derives every one of those from the peer's
/// [`ClientIdentity`](super::super::client_identity::ClientIdentity),
/// its process ancestry / registered launcher session, the verified
/// package, and the installed manifest. This is the Access::User
/// counterpart to the private-task-host [`AppServiceCall`], and it can
/// never mint a private Task Host principal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppServiceCliCall {
    pub app_id: Name,
    pub tool: Text<LABEL_BYTES>,
    pub arguments: super::bounded::McpArguments,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSessionRegister {
    pub app_id: Name,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<TextList<128, PATH_BYTES>>,
    /// The launcher's own capability set, used only to narrow what the
    /// daemon already resolved for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_caps: Option<Structured>,
    pub package: Structured,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSessionRegisterNative {
    pub app_id: Name,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSessionRegister {
    pub command: Text<COMMAND_BYTES>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_caps: Option<Structured>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSessionBind {
    pub session_id: Token,
    pub handle: Token,
    pub pid: u32,
}

pub type AppGuiLaunch = crate::clawd::gui::inputs::LaunchRequest;
pub type AppGuiWait = crate::clawd::gui::inputs::InstanceRequest;
pub type AppGuiStop = crate::clawd::gui::inputs::InstanceRequest;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSessionSetTransient {
    pub session_id: Token,
    pub handle: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_digest: Option<Token>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSessionRelay {
    pub session_id: Token,
    pub handle: Token,
    /// Wire name of the inner route. Resolved against the one typed
    /// route registry; anything the registry does not name, or names as
    /// something other than a `Session`-subject system-service route,
    /// is refused before its body is decoded.
    pub command: Text<COMMAND_BYTES>,
    /// Bounded business data, including full filesystem text. The inner
    /// route's typed decoder reapplies its field-specific limits before
    /// authorization; this transport allowance grants no broader authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<super::bounded::McpArguments>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSessionDeregister {
    pub session_id: Token,
    pub handle: Token,
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerRun {
    pub subsystem: Token,
    pub command: Token,
    /// The `cos cron` / `cos triggers` argument vector. `scheduler`
    /// re-validates it against the allow-listed command before the job
    /// or rule id it addresses is resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Structured>,
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialOauthRefresh {
    pub session: Token,
    pub namespace: Name,
    pub credential: Name,
}

// ---------------------------------------------------------------------------
// System services
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileState {
    #[serde(deserialize_with = "file_state_sha256")]
    pub sha256: String,
    #[serde(deserialize_with = "file_state_size")]
    pub size: u64,
    pub device: u64,
    pub inode: u64,
    /// Full st_mode, including the regular-file type bits.
    pub mode: u32,
    pub modified_ns: i64,
    pub changed_ns: i64,
}

fn file_state_sha256<'de, D: serde::Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    let value = Text::<71>::deserialize(de)?;
    let Some(hash) = value.as_str().strip_prefix("sha256:") else {
        return Err(serde::de::Error::custom("invalid file SHA-256"));
    };
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(serde::de::Error::custom("invalid file SHA-256"));
    }
    Ok(value.as_str().to_string())
}

fn file_state_size<'de, D: serde::Deserializer<'de>>(de: D) -> Result<u64, D::Error> {
    let size = u64::deserialize(de)?;
    if size > 65_536 {
        return Err(serde::de::Error::custom("file state exceeds 64 KiB"));
    }
    Ok(size)
}

fn required_file_state<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> Result<Option<FileState>, D::Error> {
    Option::<FileState>::deserialize(de)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileReplace {
    pub session: Token,
    pub path: Text<PATH_BYTES>,
    // An omitted precondition is not an assertion that the target is absent.
    #[serde(deserialize_with = "required_file_state")]
    pub expected: Option<FileState>,
    pub content_base64: Text<90_000>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Name>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessibilityControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Text<LABEL_BYTES>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_daily: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_weekly: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_monthly: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BluetoothControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_serial: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipboardControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrashInspect {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_minutes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_url: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<Text<COMMAND_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Text<BROWSER_VALUE_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<Text<BROWSER_VALUE_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Token>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uris: Option<TextList<32, PATH_BYTES>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemAccess {
    pub session: Token,
    pub request: FilesystemOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotCapture {
    pub session: Token,
    pub request: ScreenshotRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotRequest {
    pub directory: Text<PATH_BYTES>,
    pub modal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaPlayerControl {
    pub session: Token,
    pub action: MediaPlayerAction,
    pub deadline_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarDay {
    pub session: Token,
    pub year: i16,
    pub month: u8,
    pub day: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaPlayerAction {
    Status,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    Toggle,
}

impl MediaPlayerAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Stop => "stop",
            Self::Next => "next",
            Self::Previous => "previous",
            Self::Toggle => "toggle",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiChat {
    pub session: Token,
    pub app_id: Name,
    pub origin: Token,
    pub prompt: super::bounded::FileText,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<super::bounded::FileText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_units: Option<u64>,
    pub tools: TextList<64, 256>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum FilesystemOperation {
    Read {
        path: Text<PATH_BYTES>,
    },
    Write {
        path: Text<PATH_BYTES>,
        content: super::bounded::FileText,
    },
    Replace {
        path: Text<PATH_BYTES>,
        find: super::bounded::FileText,
        replace: super::bounded::FileText,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adaptive_sync: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backlight: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirewallControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_action: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionAction {
    pub session: Token,
    pub action: Token,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocationQuery {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accuracy: Option<Token>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<Name>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkDiagnose {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageInstall {
    pub session: Token,
    /// Overwritten with `install` by the route before dispatch; kept
    /// declarable so an existing caller that sends it is not refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Name>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Name>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRestore {
    pub session: Token,
    pub mutation_session: Token,
    pub mutation_seq: u64,
    pub package: Name,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<Name>,
    pub was_held: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PowerControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrinterControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub printer: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sides: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copies: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<Name>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceRestore {
    pub session: Token,
    pub mutation_session: Token,
    pub mutation_seq: u64,
    pub unit: Name,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<Name>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsbControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsersControl {
    pub session: Token,
    pub action: Token,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_name: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<Text<PATH_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<Text<LABEL_BYTES>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/wire/requests.rs"
    ));
}

// ---------------------------------------------------------------------------
// Session journal
// ---------------------------------------------------------------------------

/// Ask what the session journal has to say about the caller's own work.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalStatus {
    /// Restrict the answer to one durable session. Omitted means the
    /// caller's own owner partition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Token>,
}

/// Record what an operator concluded about a mutation the machine could
/// not resolve on its own.
///
/// Root-only, and bound to an exact partition and operation: the route
/// resolves nothing it was not told to, re-runs nothing, and grants
/// nothing. `outcome` is an enumerated statement, not an action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalResolveMutation {
    /// Partition key exactly as the journal renders it, e.g.
    /// `owner/1000` or `session/ses_…`.
    pub partition: Name,
    /// The operation id the journal minted when the bracket opened.
    pub operation: Token,
    /// `abandoned`, `committed` or `rolled-back`.
    pub outcome: Token,
}
