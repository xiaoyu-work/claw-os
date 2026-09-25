//! Frames exchanged between `clawd` and one `claw-agentd` worker.
//!
//! The channel is a private `socketpair(2)` handed to the child as fd
//! 3 and carries newline-delimited JSON. It exposes nothing but the
//! lifecycle of the single task the worker was spawned for, including a
//! closed App operation/session control surface. There is no general broker
//! proxy, admin, scheduler or permission-decision route here, and
//! every payload is a typed, already policy-projected structure rather
//! than free-form JSON, so a compromised worker cannot widen what it
//! reports.
//!
//! Both sides check [`PROTOCOL_VERSION`]. A mixed old/new install
//! therefore fails closed with a named error instead of silently
//! mis-parsing a frame.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

use crate::agent::llm::{ProviderFallbackState, StreamEvent};
use crate::agent::runtime::evidence::EvidenceReport;
use crate::agent::service::FinishOutcome;
use crate::audit_policy::{TextDigest, ToolFacts};
use crate::caps::{ConsentContext, Scope};
use crate::proc::SessionInfo;

use super::grant::SignedGrant;

/// Bumped whenever a frame changes shape. `clawd` refuses a worker that
/// reports a different version, and the worker refuses an assignment
/// that carries one.
pub const PROTOCOL_VERSION: u32 = 16;

/// Descriptor the broker dups the worker end of the channel onto.
pub const CHANNEL_FD: i32 = 3;

/// Set in the worker environment so the child can assert it received a
/// channel rather than guessing at fd 3.
pub const CHANNEL_FD_ENV: &str = "COS_AGENTD_CHANNEL_FD";

/// Hard cap on a single frame. Streaming deltas and final answers are
/// far below this; anything larger is treated as a protocol fault and
/// closes the channel.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Bound on the identifiers a worker may echo back into persisted
/// progress records.
const MAX_PROGRESS_FIELD_CHARS: usize = 128;

pub const ROUTE_HELLO: &str = "hello";
pub const ROUTE_PREPARED: &str = "prepared";
pub const ROUTE_STREAM: &str = "stream";
pub const ROUTE_PROGRESS: &str = "progress";
pub const ROUTE_AUDIT: &str = "audit";
pub const ROUTE_HEARTBEAT: &str = "heartbeat";
pub const ROUTE_RESULT: &str = "result";
/// Permission mediation. The only way a worker can reach the consent
/// system: it may name the exact verb and canonical scope it was denied
/// plus an optional digest of validated operation inputs — never a
/// session, an owner, a decision, raw arguments, or a capability set.
pub const ROUTE_APPROVAL: &str = "approval";
pub const ROUTE_RECEIPT: &str = "receipt";
pub const ROUTE_MONETARY_BUDGET: &str = "monetary_budget";

/// The complete route surface a worker grant may carry. Nothing else
/// exists on this channel, so a leaked descriptor is still only an
/// authority to report on one task.
pub const WORKER_ROUTES: &[&str] = &[
    ROUTE_PREPARED,
    ROUTE_HELLO,
    ROUTE_STREAM,
    ROUTE_PROGRESS,
    ROUTE_AUDIT,
    ROUTE_HEARTBEAT,
    ROUTE_RESULT,
    ROUTE_APPROVAL,
    ROUTE_RECEIPT,
    ROUTE_MONETARY_BUDGET,
];

/// Hard ceiling on permission mediation for one task, so a looping
/// model cannot flood the consent store or the broker.
pub const MAX_APPROVAL_ASKS: u32 = 128;
pub const MAX_BOUNDARY_CHECKS: u32 = 4096;
pub const MAX_RECEIPT_REPORTS: u32 = 128;
pub const MAX_RECEIPT_REPORT_BYTES: usize = 16 * 1024;
pub const MAX_MONETARY_EXCHANGES: u32 = 256;

pub fn worker_routes() -> Vec<String> {
    WORKER_ROUTES
        .iter()
        .map(|route| (*route).to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Broker → worker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrokerFrame {
    Prepare(Box<Assignment>),
    Commit(Box<ExecutionCommit>),
    /// Authenticated acknowledgement that the supervisor renewed the task
    /// Host's rolling lease after receiving a worker heartbeat.
    ExtensionLeaseRenewed {
        task_id: String,
        expires_at_ms: u64,
    },
    /// Cooperative cancellation. The supervisor still escalates to
    /// `SIGKILL` across the worker's process group if it does not wind
    /// down.
    Cancel {
        task_id: String,
    },
    /// Daemon shutdown: finish or abandon promptly.
    Shutdown,
    /// Answer to exactly one [`WorkerFrame::Approval`], correlated by
    /// the id the worker chose. Carries no capability and no decision
    /// authority — only whether the gate may proceed and, for a filed
    /// request, the safe id the user will act on.
    ApprovalReply {
        correlation_id: u64,
        exchange: ApprovalExchange,
        reply: ApprovalReply,
    },
    ReceiptReply {
        correlation_id: u64,
        reply: ReceiptReply,
    },
    MonetaryBudgetReply {
        correlation_id: u64,
        reply: MonetaryBudgetReply,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MonetaryBudgetReply {
    Reserved {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reservation: Option<Box<crate::activities::MonetaryReservation>>,
    },
    Settled,
    Refused {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum MonetaryBudgetOperation {
    Reserve {
        call_id: String,
        turn_index: u32,
        input_upper_bound_tokens: u64,
        requested_max_output_tokens: u32,
    },
    Settle {
        call_id: String,
        turn_index: u32,
        settlement: crate::activities::MonetarySettlement,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonetaryBudgetRequest {
    pub task_id: String,
    pub correlation_id: u64,
    pub operation: MonetaryBudgetOperation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReceiptReply {
    Recorded { receipt_id: String },
    Refused { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptRequest {
    pub task_id: String,
    pub correlation_id: u64,
    #[serde(deserialize_with = "receipt_report")]
    pub report: Box<crate::activities::ReceiptReport>,
}

pub fn validate_receipt_report(report: &crate::activities::ReceiptReport) -> Result<(), String> {
    report.validate().map_err(|error| error.to_string())?;
    let encoded = serde_json::to_vec(report).map_err(|error| error.to_string())?;
    if encoded.len() > MAX_RECEIPT_REPORT_BYTES {
        return Err("receipt report exceeds 16 KiB".to_string());
    }
    Ok(())
}

fn receipt_report<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Box<crate::activities::ReceiptReport>, D::Error> {
    let report = crate::activities::ReceiptReport::deserialize(deserializer)?;
    validate_receipt_report(&report).map_err(serde::de::Error::custom)?;
    Ok(Box::new(report))
}

/// What a worker may say when a capability check fails: the exact verb,
/// canonical scope, and optional digest of already-validated operation
/// inputs. Session, owner, task and worker identity are never sent — the
/// broker takes all four from the verified grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApprovalAsk {
    /// Read a live Activity constraint without consuming or filing consent.
    Boundary { verb: String, scope: Scope },
    /// Spend an already-approved, exactly-matching grant. One-shot: the
    /// broker consumes it, so a replay finds nothing.
    Consume {
        verb: String,
        scope: Scope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation_digest: Option<String>,
    },
    /// File (or reuse) a pending request for this exact verb and scope.
    Request {
        verb: String,
        scope: Scope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation_digest: Option<String>,
    },
}

impl ApprovalAsk {
    pub fn verb(&self) -> &str {
        match self {
            ApprovalAsk::Boundary { verb, .. }
            | ApprovalAsk::Consume { verb, .. }
            | ApprovalAsk::Request { verb, .. } => verb.as_str(),
        }
    }

    pub fn scope(&self) -> &Scope {
        match self {
            ApprovalAsk::Boundary { scope, .. }
            | ApprovalAsk::Consume { scope, .. }
            | ApprovalAsk::Request { scope, .. } => scope,
        }
    }

    pub fn operation_digest(&self) -> Option<&str> {
        match self {
            ApprovalAsk::Boundary { .. } => None,
            ApprovalAsk::Consume {
                operation_digest, ..
            }
            | ApprovalAsk::Request {
                operation_digest, ..
            } => operation_digest.as_deref(),
        }
    }
}

/// Unpredictable, exact binding for one approval round trip.
///
/// Correlation ids are only counters and are not authenticators. The broker
/// echoes this whole value, and the worker accepts the reply only when the
/// nonce, verb, scope, operation digest, and ask kind all match its waiter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalExchange {
    pub nonce: String,
    pub ask: ApprovalAsk,
}

impl ApprovalExchange {
    pub fn new(ask: ApprovalAsk) -> Self {
        Self {
            nonce: uuid::Uuid::new_v4().simple().to_string(),
            ask,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.nonce.len() == 32
            && self
                .nonce
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApprovalReply {
    /// A constraint only; this cannot satisfy an approval-consumption waiter.
    Boundary {
        decision: crate::activities::CapabilityBoundaryDecision,
    },
    /// An exact approved grant existed and has been spent.
    Granted,
    /// No grant to spend, or the request is still waiting on the user.
    /// `request_id` is a bounded identifier, never authority.
    Pending {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    /// The broker refused to mediate — unknown verb, unusable scope,
    /// no session on the lease, budget exhausted, or the consent store
    /// is unavailable. The gate stays closed.
    Refused { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    pub protocol: u32,
    pub grant: SignedGrant,
    pub job: JobSpec,
    /// Broker-derived execution context. The worker uses this only to
    /// explain denials; the broker independently enforces it whenever
    /// consent is requested.
    #[serde(default = "unattended_consent")]
    pub consent_context: ConsentContext,
    /// Session scope the *broker* derived. Capabilities are never taken
    /// from the worker; they are re-derived by `clawd` from root-owned
    /// session metadata and installed in the worker as a task-local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionInfo>,
    /// Ephemeral, broker-authenticated proof that the submitting client was
    /// recently present. Never persisted in the task/session record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<crate::session::SessionPresence>,
    /// Broker-spawned task-owned extension host. Its complete identity and
    /// channel paths are also signed into the worker grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<crate::extension_host::protocol::ExtensionBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionCommit {
    pub protocol: u32,
    pub grant: SignedGrant,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub worker_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_start_time_ticks: Option<u64>,
    pub capability_generation: String,
    pub prepare_nonce: String,
    pub commit_nonce: String,
}

fn unattended_consent() -> ConsentContext {
    ConsentContext::Unattended
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::agent::attachments::ImageAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_reasoning_effort: Option<String>,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub use_memory: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub plan_only: bool,
    pub owner_uid: u32,
    pub owner_home: String,
    /// Broker-validated canonical directory under `owner_home`. It selects
    /// runtime context but grants no capability.
    pub workspace: String,
    /// Reporting hint only; the broker resolves the Activity from its own Job.
    #[serde(default)]
    pub record_activity_receipts: bool,
    /// The ordinary worker checks each capability through its private channel.
    /// Root still derives the actual policy from its own Job, not this hint.
    #[serde(default)]
    pub activity_capability_checks: bool,
    /// Reporting hint only; Root derives owner, Activity, Job and session.
    #[serde(default)]
    pub activity_monetary_checks: bool,
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

// ---------------------------------------------------------------------------
// Worker → broker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerFrame {
    Prepared(Box<WorkerPrepared>),
    Hello(Box<WorkerHello>),
    Stream {
        task_id: String,
        event: Box<StreamEvent>,
    },
    Progress {
        task_id: String,
        progress: ProgressRecord,
    },
    Audit {
        task_id: String,
        record: Box<RuntimeAuditRecord>,
    },
    Heartbeat {
        task_id: String,
    },
    /// Permission mediation for one denied capability check.
    Approval {
        task_id: String,
        correlation_id: u64,
        exchange: ApprovalExchange,
    },
    Receipt(Box<ReceiptRequest>),
    MonetaryBudget(Box<MonetaryBudgetRequest>),
    Result {
        task_id: String,
        outcome: Box<WorkerOutcome>,
    },
}

impl WorkerFrame {
    pub fn route(&self) -> &'static str {
        match self {
            WorkerFrame::Prepared(_) => ROUTE_PREPARED,
            WorkerFrame::Hello(_) => ROUTE_HELLO,
            WorkerFrame::Stream { .. } => ROUTE_STREAM,
            WorkerFrame::Progress { .. } => ROUTE_PROGRESS,
            WorkerFrame::Audit { .. } => ROUTE_AUDIT,
            WorkerFrame::Heartbeat { .. } => ROUTE_HEARTBEAT,
            WorkerFrame::Approval { .. } => ROUTE_APPROVAL,
            WorkerFrame::Receipt(_) => ROUTE_RECEIPT,
            WorkerFrame::MonetaryBudget(_) => ROUTE_MONETARY_BUDGET,
            WorkerFrame::Result { .. } => ROUTE_RESULT,
        }
    }

    /// Task the frame claims to be about. `hello` carries it inside the
    /// grant instead, which the broker authenticates separately.
    pub fn task_id(&self) -> Option<&str> {
        match self {
            WorkerFrame::Prepared(prepared) => Some(prepared.grant.claims.task_id.as_str()),
            WorkerFrame::Hello(hello) => Some(hello.grant.claims.task_id.as_str()),
            WorkerFrame::Receipt(request) => Some(request.task_id.as_str()),
            WorkerFrame::MonetaryBudget(request) => Some(request.task_id.as_str()),
            WorkerFrame::Stream { task_id, .. }
            | WorkerFrame::Progress { task_id, .. }
            | WorkerFrame::Audit { task_id, .. }
            | WorkerFrame::Heartbeat { task_id }
            | WorkerFrame::Approval { task_id, .. }
            | WorkerFrame::Result { task_id, .. } => Some(task_id.as_str()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerPrepared {
    pub protocol: u32,
    pub grant: SignedGrant,
    pub prepare_nonce: String,
    pub commit_nonce: String,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time_ticks: Option<u64>,
    pub uid: u32,
    pub gid: u32,
}

/// Self-report the worker makes once it has verified the assignment.
/// The broker does not *trust* these fields — it re-derives uid, pid and
/// start-time from the kernel — but a mismatch is a loud signal that
/// privilege dropping did not take effect, so the task is failed rather
/// than run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerHello {
    pub protocol: u32,
    pub security_epoch: u64,
    pub grant: SignedGrant,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time_ticks: Option<u64>,
    pub uid: u32,
    pub euid: u32,
    pub gid: u32,
    pub egid: u32,
    pub supplementary_groups: Vec<u32>,
    pub no_new_privs: bool,
    pub dumpable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProgressRecord {
    ToolStart {
        id: String,
        name: String,
    },
    ToolResult {
        id: String,
        name: String,
        ok: bool,
        latency_ms: u64,
    },
}

impl ProgressRecord {
    /// Project into the exact JSON shape `agent service` has always
    /// appended to a task stream, with identifiers clamped so a worker
    /// cannot inflate the persisted record.
    pub fn to_stream_value(&self) -> Value {
        match self {
            ProgressRecord::ToolStart { id, name } => json!({
                "kind": "tool_start",
                "id": clamp(id),
                "name": clamp(name),
            }),
            ProgressRecord::ToolResult {
                id,
                name,
                ok,
                latency_ms,
            } => json!({
                "kind": "tool_result",
                "id": clamp(id),
                "name": clamp(name),
                "ok": ok,
                "latency_ms": latency_ms,
            }),
        }
    }
}

fn clamp(value: &str) -> String {
    value.chars().take(MAX_PROGRESS_FIELD_CHARS).collect()
}

/// Runtime audit the worker forwards so the model-visible tool and turn
/// trail stays reconstructable from the root-owned `clawd` audit log.
/// Every field is a value `audit_policy` already projected, and the
/// broker re-projects tool facts on receipt, so nothing model-authored
/// reaches the log through this route.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RuntimeAuditRecord {
    ToolStarted {
        session_id: String,
        turn_index: u32,
        tool: ToolFacts,
        tool_use_id: String,
    },
    ToolFinished {
        session_id: String,
        turn_index: u32,
        tool: ToolFacts,
        tool_use_id: String,
        success: bool,
        latency_ms: u64,
        bytes_returned: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<TextDigest>,
    },
    TurnFinished {
        session_id: String,
        turn_index: u32,
        provider: String,
        model: String,
        success: bool,
        latency_ms: u64,
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: u32,
        cache_write_tokens: u32,
        tool_calls_made: u32,
        stop_reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<TextDigest>,
    },
    ExtensionLifecycle {
        session_id: String,
        kind: crate::extension_host::protocol::ExtensionKind,
        action: crate::extension_host::protocol::LifecycleAction,
        extension_id: String,
        binding_digest: String,
        lease_digest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage: Option<crate::extension_host::protocol::AuditStage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app: Option<Box<crate::extension_host::protocol::AppInvocationAudit>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mcp: Option<crate::extension_host::protocol::McpInvocationAudit>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        abi: Option<Box<crate::extension_host::protocol::AgentExtensionAudit>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        manifest_digest: Option<String>,
        success: bool,
        latency_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<TextDigest>,
    },
}

impl RuntimeAuditRecord {
    pub fn session_id(&self) -> &str {
        match self {
            RuntimeAuditRecord::ToolStarted { session_id, .. }
            | RuntimeAuditRecord::ToolFinished { session_id, .. }
            | RuntimeAuditRecord::TurnFinished { session_id, .. }
            | RuntimeAuditRecord::ExtensionLifecycle { session_id, .. } => session_id.as_str(),
        }
    }
}

/// A finished run, boxed so the success payload does not inflate every
/// frame that merely reports an error or a cancellation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedRun {
    pub response: String,
    pub turns_used: u32,
    pub provider: String,
    pub model: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "evidence_wire"
    )]
    pub evidence: Option<EvidenceReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<ProviderFallbackState>,
}

// `arbitrary_precision` buffers JSON floats as maps while decoding tagged
// enums. Keep only the AgentD wire representation decimal-string based.
mod evidence_wire {
    use serde::de::Error as _;
    use serde::ser::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};
    use serde_json::{Map, Number, Value};

    use crate::agent::runtime::evidence::EvidenceReport;

    const REPORT_CONFIDENCE_FIELDS: &[&str] = &["binding_confidence", "claim_confidence"];
    const CLAIM_CONFIDENCE_FIELDS: &[&str] = &["declared_confidence", "effective_confidence"];
    const MAX_CONFIDENCE_CHARS: usize = 64;

    pub fn serialize<S>(report: &Option<EvidenceReport>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let Some(report) = report else {
            return serializer.serialize_none();
        };
        let mut wire = serde_json::to_value(report).map_err(S::Error::custom)?;
        encode_confidences(&mut wire).map_err(S::Error::custom)?;
        serializer.serialize_some(&wire)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<EvidenceReport>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let Some(mut wire) = Option::<Value>::deserialize(deserializer)? else {
            return Ok(None);
        };
        decode_confidences(&mut wire).map_err(D::Error::custom)?;
        serde_json::from_value(wire)
            .map(Some)
            .map_err(D::Error::custom)
    }

    fn encode_confidences(report: &mut Value) -> Result<(), String> {
        let report = report
            .as_object_mut()
            .ok_or_else(|| "evidence report is not an object".to_string())?;
        for field in REPORT_CONFIDENCE_FIELDS {
            encode_field(report, field)?;
        }
        for claim in claims_mut(report)? {
            let claim = claim
                .as_object_mut()
                .ok_or_else(|| "evidence claim is not an object".to_string())?;
            for field in CLAIM_CONFIDENCE_FIELDS {
                encode_field(claim, field)?;
            }
        }
        Ok(())
    }

    fn decode_confidences(report: &mut Value) -> Result<(), String> {
        let report = report
            .as_object_mut()
            .ok_or_else(|| "evidence report is not an object".to_string())?;
        for field in REPORT_CONFIDENCE_FIELDS {
            decode_field(report, field)?;
        }
        for claim in claims_mut(report)? {
            let claim = claim
                .as_object_mut()
                .ok_or_else(|| "evidence claim is not an object".to_string())?;
            for field in CLAIM_CONFIDENCE_FIELDS {
                decode_field(claim, field)?;
            }
        }
        Ok(())
    }

    fn claims_mut(report: &mut Map<String, Value>) -> Result<&mut Vec<Value>, String> {
        report
            .get_mut("claims")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| "evidence claims are not an array".to_string())
    }

    fn encode_field(object: &mut Map<String, Value>, field: &str) -> Result<(), String> {
        let Some(value) = object.get_mut(field) else {
            return Ok(());
        };
        let encoded = match value {
            Value::Number(number) if number.as_f64().is_some_and(f64::is_finite) => {
                number.to_string()
            }
            Value::Number(_) => return Err(format!("evidence {field} is not finite")),
            _ => return Err(format!("evidence {field} is not numeric")),
        };
        *value = Value::String(encoded);
        Ok(())
    }

    fn decode_field(object: &mut Map<String, Value>, field: &str) -> Result<(), String> {
        let Some(value) = object.get_mut(field) else {
            return Ok(());
        };
        let Value::String(encoded) = value else {
            return Err(format!("evidence {field} is not a wire decimal"));
        };
        if encoded.len() > MAX_CONFIDENCE_CHARS {
            return Err(format!("evidence {field} wire decimal is too long"));
        }
        let confidence = encoded
            .parse::<f64>()
            .map_err(|_| format!("evidence {field} is not a valid wire decimal"))?;
        if !confidence.is_finite() {
            return Err(format!("evidence {field} is not finite"));
        }
        *value = Value::Number(
            Number::from_f64(confidence)
                .ok_or_else(|| format!("evidence {field} is not finite"))?,
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkerOutcome {
    Ok(Box<CompletedRun>),
    Error { message: String },
    Cancelled,
    WaitingApproval { request_ids: Vec<String> },
}

impl From<WorkerOutcome> for FinishOutcome {
    fn from(outcome: WorkerOutcome) -> Self {
        match outcome {
            WorkerOutcome::Ok(run) => {
                let CompletedRun {
                    response,
                    turns_used,
                    provider,
                    model,
                    evidence,
                    fallback,
                } = *run;
                FinishOutcome::Ok {
                    response,
                    turns_used,
                    provider,
                    model,
                    evidence: Box::new(evidence),
                    fallback: Box::new(fallback),
                }
            }
            WorkerOutcome::Error { message } => FinishOutcome::Error(message),
            WorkerOutcome::Cancelled => FinishOutcome::Cancelled,
            WorkerOutcome::WaitingApproval { request_ids } => {
                FinishOutcome::WaitingApproval { request_ids }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

pub fn encode<T: Serialize>(frame: &T) -> Result<String, String> {
    let mut line =
        serde_json::to_string(frame).map_err(|error| format!("encode agentd frame: {error}"))?;
    if line.len() > MAX_FRAME_BYTES {
        return Err(format!(
            "agentd frame is {} bytes; maximum is {MAX_FRAME_BYTES}",
            line.len()
        ));
    }
    line.push('\n');
    Ok(line)
}

/// Newline framing with a hard per-frame ceiling, so a peer cannot make
/// the reader allocate without bound before the size is known.
pub struct FrameReader<R> {
    inner: R,
    buf: Vec<u8>,
}

impl<R: AsyncBufRead + Unpin> FrameReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buf: Vec::new(),
        }
    }

    pub async fn next_frame<T: for<'de> Deserialize<'de>>(&mut self) -> Result<Option<T>, String> {
        loop {
            self.buf.clear();
            let read = {
                let mut limited = (&mut self.inner).take(MAX_FRAME_BYTES as u64 + 1);
                limited
                    .read_until(b'\n', &mut self.buf)
                    .await
                    .map_err(|error| format!("read agentd frame: {error}"))?
            };
            if read == 0 {
                return Ok(None);
            }
            if !self.buf.ends_with(b"\n") {
                return Err(format!(
                    "agentd frame exceeded {MAX_FRAME_BYTES} bytes without a terminator"
                ));
            }
            let line = std::str::from_utf8(&self.buf)
                .map_err(|_| "agentd frame is not valid UTF-8".to_string())?
                .trim();
            if line.is_empty() {
                continue;
            }
            let frame = serde_json::from_str::<T>(line)
                .map_err(|error| format!("decode agentd frame: {error}"))?;
            return Ok(Some(frame));
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/protocol.rs"
    ));
}
