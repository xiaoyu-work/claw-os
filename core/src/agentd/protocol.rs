//! Frames exchanged between `clawd` and one `claw-agentd` worker.
//!
//! The channel is a private `socketpair(2)` handed to the child as fd
//! 3 and carries newline-delimited JSON. It exposes nothing but the
//! lifecycle of the single task the worker was spawned for: there is no
//! admin, App-session, scheduler or permission-decision route here, and
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
pub const PROTOCOL_VERSION: u32 = 4;

/// Descriptor the broker dups the worker end of the channel onto.
pub const CHANNEL_FD: i32 = 3;

/// Set in the worker environment so the child can assert it received a
/// channel rather than guessing at fd 3.
pub const CHANNEL_FD_ENV: &str = "COS_AGENTD_CHANNEL_FD";
/// Bootstrap-only task hint retained for compatibility with older launchers.
/// A current worker does not consume it and removes it before running tools.
pub const TASK_HINT_ENV: &str = "COS_AGENTD_TASK";

/// Hard cap on a single frame. Streaming deltas and final answers are
/// far below this; anything larger is treated as a protocol fault and
/// closes the channel.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Bound on the identifiers a worker may echo back into persisted
/// progress records.
const MAX_PROGRESS_FIELD_CHARS: usize = 128;

pub const ROUTE_HELLO: &str = "hello";
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

/// The complete route surface a worker grant may carry. Nothing else
/// exists on this channel, so a leaked descriptor is still only an
/// authority to report on one task.
pub const WORKER_ROUTES: &[&str] = &[
    ROUTE_HELLO,
    ROUTE_STREAM,
    ROUTE_PROGRESS,
    ROUTE_AUDIT,
    ROUTE_HEARTBEAT,
    ROUTE_RESULT,
    ROUTE_APPROVAL,
];

/// Hard ceiling on permission mediation for one task, so a looping
/// model cannot flood the consent store or the broker.
pub const MAX_APPROVAL_ASKS: u32 = 128;

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
    Assign(Box<Assignment>),
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
}

/// What a worker may say when a capability check fails: the exact verb,
/// canonical scope, and optional digest of already-validated operation
/// inputs. Session, owner, task and worker identity are never sent — the
/// broker takes all four from the verified grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "snake_case")]
pub enum ApprovalAsk {
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
            ApprovalAsk::Consume { verb, .. } | ApprovalAsk::Request { verb, .. } => verb.as_str(),
        }
    }

    pub fn scope(&self) -> &Scope {
        match self {
            ApprovalAsk::Consume { scope, .. } | ApprovalAsk::Request { scope, .. } => scope,
        }
    }

    pub fn operation_digest(&self) -> Option<&str> {
        match self {
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
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ApprovalReply {
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
}

fn unattended_consent() -> ConsentContext {
    ConsentContext::Unattended
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    pub owner_uid: u32,
    pub owner_home: String,
}

// ---------------------------------------------------------------------------
// Worker → broker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerFrame {
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
    Result {
        task_id: String,
        outcome: Box<WorkerOutcome>,
    },
}

impl WorkerFrame {
    pub fn route(&self) -> &'static str {
        match self {
            WorkerFrame::Hello(_) => ROUTE_HELLO,
            WorkerFrame::Stream { .. } => ROUTE_STREAM,
            WorkerFrame::Progress { .. } => ROUTE_PROGRESS,
            WorkerFrame::Audit { .. } => ROUTE_AUDIT,
            WorkerFrame::Heartbeat { .. } => ROUTE_HEARTBEAT,
            WorkerFrame::Approval { .. } => ROUTE_APPROVAL,
            WorkerFrame::Result { .. } => ROUTE_RESULT,
        }
    }

    /// Task the frame claims to be about. `hello` carries it inside the
    /// grant instead, which the broker authenticates separately.
    pub fn task_id(&self) -> Option<&str> {
        match self {
            WorkerFrame::Hello(hello) => Some(hello.grant.claims.task_id.as_str()),
            WorkerFrame::Stream { task_id, .. }
            | WorkerFrame::Progress { task_id, .. }
            | WorkerFrame::Audit { task_id, .. }
            | WorkerFrame::Heartbeat { task_id }
            | WorkerFrame::Approval { task_id, .. }
            | WorkerFrame::Result { task_id, .. } => Some(task_id.as_str()),
        }
    }
}

/// Self-report the worker makes once it has verified the assignment.
/// The broker does not *trust* these fields — it re-derives uid, pid and
/// start-time from the kernel — but a mismatch is a loud signal that
/// privilege dropping did not take effect, so the task is failed rather
/// than run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerHello {
    pub protocol: u32,
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
}

impl RuntimeAuditRecord {
    pub fn session_id(&self) -> &str {
        match self {
            RuntimeAuditRecord::ToolStarted { session_id, .. }
            | RuntimeAuditRecord::ToolFinished { session_id, .. }
            | RuntimeAuditRecord::TurnFinished { session_id, .. } => session_id.as_str(),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<EvidenceReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<ProviderFallbackState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkerOutcome {
    Ok(Box<CompletedRun>),
    Error { message: String },
    Cancelled,
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
