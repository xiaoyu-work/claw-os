use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use serde_json::json;

use crate::agent::llm::ToolCall;
use crate::agent::runtime::hooks::{
    self, Hook, HookContext, HookOutcome, ToolDecision, ToolResultSummary, TurnSummary,
};
use crate::audit_policy::{
    self, InvalidRequestFacts, RequestFacts, ResponseFacts, TextDigest, ToolFacts,
};
use crate::session::{self, Mutation, MutationRecord, SessionId};

use super::client_identity::ClientIdentity;

#[derive(Debug, Serialize)]
struct RequestAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    #[serde(flatten)]
    request: &'a RequestFacts,
    #[serde(flatten)]
    outcome: &'a ResponseFacts,
    duration_ms: u128,
    client: &'a ClientIdentity,
}

#[derive(Debug, Serialize)]
struct InvalidRequestAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    #[serde(flatten)]
    request: &'a InvalidRequestFacts,
    #[serde(flatten)]
    outcome: &'a ResponseFacts,
    duration_ms: u128,
    client: &'a ClientIdentity,
}

#[derive(Debug, Serialize)]
struct TaskAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    job_id: String,
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_start_time_ticks: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<TextDigest>,
}

#[derive(Debug, Serialize)]
struct RuntimeTurnAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
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
    stop_reason: &'a str,
    /// Provider and tool failures quote request bodies, headers and
    /// caller arguments, so the text never reaches the log.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<TextDigest>,
}

#[derive(Debug, Serialize)]
struct RuntimeToolAudit {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    session_id: String,
    turn_index: u32,
    #[serde(flatten)]
    tool: ToolFacts,
    tool_use_id: String,
    success: bool,
    latency_ms: u64,
    bytes_returned: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<TextDigest>,
}

/// Append the broker's record of a dispatched request.
///
/// Both this log and [`super::system_journal`] are handed the same
/// [`RequestFacts`] and [`ResponseFacts`], which only
/// [`crate::audit_policy`] can build — neither sink ever sees the raw
/// `params`, so one cannot mask a value the other writes in the clear.
pub fn record_request(
    request: &RequestFacts,
    outcome: &ResponseFacts,
    duration: Duration,
    client: &ClientIdentity,
) -> Result<(), String> {
    let audit = RequestAudit {
        ts: Utc::now(),
        event: "clawd.request",
        request,
        outcome,
        duration_ms: duration.as_millis(),
        client,
    };
    append_jsonl(&audit)
}

pub fn record_invalid(
    request: &InvalidRequestFacts,
    outcome: &ResponseFacts,
    duration: Duration,
    client: &ClientIdentity,
) -> Result<(), String> {
    let audit = InvalidRequestAudit {
        ts: Utc::now(),
        event: "clawd.invalid-request",
        request,
        outcome,
        duration_ms: duration.as_millis(),
        client,
    };
    append_jsonl(&audit)
}

pub fn record_task_event(event: &'static str, job: &crate::agent::service::Job) {
    let audit = TaskAudit {
        ts: Utc::now(),
        event,
        job_id: audit_policy::safe_identity(&job.id),
        status: job.status.as_str(),
        session_id: job.session_id.as_deref().map(audit_policy::safe_identity),
        worker_pid: job.worker_pid,
        worker_start_time_ticks: job.worker_start_time_ticks,
        provider: job.provider.as_deref().map(audit_policy::safe_identity),
        model: job.model.as_deref().map(audit_policy::safe_identity),
        error: audit_policy::optional_text_digest(job.error.as_deref()),
    };
    if let Err(err) = append_jsonl(&audit) {
        tracing::error!(error = %err, event, "failed to write clawd task audit record");
    }
    super::system_journal::record_task_event(event, job);
}

pub fn install_runtime_hook() {
    hooks::global_registry().register(Arc::new(ClawdRuntimeAuditHook));
}

/// Persist runtime audit forwarded by an `agentd` worker.
///
/// The worker holds no descriptor on this log, so the record arrives as
/// a typed [`crate::agentd::protocol::RuntimeAuditRecord`] on its job
/// channel. Tool facts are re-projected through [`audit_policy`] here —
/// the worker's own projection is not trusted — and the task/owner
/// stamps come from the lease rather than from the frame, so a
/// compromised worker cannot forge either the provenance or the payload
/// of a record.
pub fn record_worker_runtime(
    task_id: &str,
    owner_uid: u32,
    record: &crate::agentd::protocol::RuntimeAuditRecord,
) {
    use crate::agentd::protocol::RuntimeAuditRecord as Record;

    let job_id = audit_policy::safe_identity(task_id);
    let result = match record {
        Record::ToolStarted {
            session_id,
            turn_index,
            tool,
            tool_use_id,
        } => append_jsonl(&WorkerToolAudit {
            ts: Utc::now(),
            event: "clawd.agent.tool.started",
            job_id: &job_id,
            owner_uid,
            session_id: audit_policy::safe_identity(session_id),
            turn_index: *turn_index,
            tool: audit_policy::reproject_tool_facts(tool),
            tool_use_id: audit_policy::safe_identity(tool_use_id),
            success: true,
            latency_ms: 0,
            bytes_returned: 0,
            error: None,
        }),
        Record::ToolFinished {
            session_id,
            turn_index,
            tool,
            tool_use_id,
            success,
            latency_ms,
            bytes_returned,
            error,
        } => append_jsonl(&WorkerToolAudit {
            ts: Utc::now(),
            event: "clawd.agent.tool.finished",
            job_id: &job_id,
            owner_uid,
            session_id: audit_policy::safe_identity(session_id),
            turn_index: *turn_index,
            tool: audit_policy::reproject_tool_facts(tool),
            tool_use_id: audit_policy::safe_identity(tool_use_id),
            success: *success,
            latency_ms: *latency_ms,
            bytes_returned: *bytes_returned,
            error: error.clone(),
        }),
        Record::TurnFinished {
            session_id,
            turn_index,
            provider,
            model,
            success,
            latency_ms,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            tool_calls_made,
            stop_reason,
            error,
        } => append_jsonl(&WorkerTurnAudit {
            ts: Utc::now(),
            event: "clawd.agent.turn.finished",
            job_id: &job_id,
            owner_uid,
            session_id: audit_policy::safe_identity(session_id),
            turn_index: *turn_index,
            provider: audit_policy::safe_identity(provider),
            model: audit_policy::safe_identity(model),
            success: *success,
            latency_ms: *latency_ms,
            input_tokens: *input_tokens,
            output_tokens: *output_tokens,
            cache_read_tokens: *cache_read_tokens,
            cache_write_tokens: *cache_write_tokens,
            tool_calls_made: *tool_calls_made,
            stop_reason: audit_policy::safe_identity(stop_reason),
            error: error.clone(),
        }),
    };
    if let Err(err) = result {
        tracing::error!(error = %err, "failed to write agentd worker audit record");
    }
}

#[derive(Debug, Serialize)]
struct WorkerApprovalAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    job_id: &'a str,
    owner_uid: u32,
    session_id: String,
    verb: String,
    scope: String,
    action: &'static str,
}

/// Record one permission mediation the broker performed for a worker.
///
/// Identity comes from the caller's verified lease, and the verb/scope
/// are re-bounded here, so the trail says exactly which consent decision
/// was spent or filed on whose behalf without trusting worker text.
pub fn record_worker_approval(
    task_id: &str,
    owner_uid: u32,
    session_id: &str,
    verb: &str,
    scope: &crate::caps::Scope,
    action: &'static str,
) {
    let job_id = audit_policy::safe_identity(task_id);
    let record = WorkerApprovalAudit {
        ts: Utc::now(),
        event: "clawd.agent.approval.mediated",
        job_id: &job_id,
        owner_uid,
        session_id: audit_policy::safe_identity(session_id),
        verb: audit_policy::safe_identity(verb),
        scope: audit_policy::safe_reference(&scope.to_string()),
        action,
    };
    if let Err(err) = append_jsonl(&record) {
        tracing::error!(error = %err, "failed to write agentd approval audit record");
    }
}

#[derive(Debug, Serialize)]
struct WorkerToolAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    job_id: &'a str,
    owner_uid: u32,
    session_id: String,
    turn_index: u32,
    #[serde(flatten)]
    tool: ToolFacts,
    tool_use_id: String,
    success: bool,
    latency_ms: u64,
    bytes_returned: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<TextDigest>,
}

#[derive(Debug, Serialize)]
struct WorkerTurnAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    job_id: &'a str,
    owner_uid: u32,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<TextDigest>,
}

#[derive(Debug)]
struct ClawdRuntimeAuditHook;

impl Hook for ClawdRuntimeAuditHook {
    fn name(&self) -> &str {
        "clawd-runtime-audit"
    }

    fn pre_tool(&self, ctx: &HookContext, tool_call: &ToolCall) -> ToolDecision {
        let audit = RuntimeToolAudit {
            ts: Utc::now(),
            event: "clawd.agent.tool.started",
            session_id: audit_policy::safe_identity(&ctx.session_id),
            turn_index: ctx.turn_index,
            tool: audit_policy::tool_facts(&tool_call.name, &tool_call.input),
            tool_use_id: audit_policy::safe_identity(&tool_call.id),
            success: true,
            latency_ms: 0,
            bytes_returned: 0,
            error: None,
        };
        if let Err(err) = append_jsonl(&audit) {
            tracing::error!(error = %err, "failed to write clawd pre-tool audit record");
        }
        ToolDecision::Allow
    }

    fn post_tool(
        &self,
        ctx: &HookContext,
        tool_call: &ToolCall,
        result: &ToolResultSummary,
    ) -> HookOutcome {
        let facts = audit_policy::tool_facts(&tool_call.name, &tool_call.input);
        let audit = RuntimeToolAudit {
            ts: Utc::now(),
            event: "clawd.agent.tool.finished",
            session_id: audit_policy::safe_identity(&ctx.session_id),
            turn_index: ctx.turn_index,
            tool: facts.clone(),
            tool_use_id: audit_policy::safe_identity(&tool_call.id),
            success: result.success,
            latency_ms: result.latency_ms,
            bytes_returned: result.bytes_returned,
            error: audit_policy::optional_text_digest(result.error.as_deref()),
        };
        if let Err(err) = append_jsonl(&audit) {
            tracing::error!(error = %err, "failed to write clawd post-tool audit record");
        }
        if result.success {
            record_tool_mutation(ctx, tool_call, &facts, result);
        }
        HookOutcome::Continue
    }

    fn post_turn(&self, ctx: &HookContext, summary: &TurnSummary) -> HookOutcome {
        let audit = RuntimeTurnAudit {
            ts: Utc::now(),
            event: "clawd.agent.turn.finished",
            session_id: audit_policy::safe_identity(&ctx.session_id),
            turn_index: ctx.turn_index,
            provider: audit_policy::safe_identity(&ctx.provider),
            model: audit_policy::safe_identity(&ctx.model),
            success: summary.success,
            latency_ms: summary.latency_ms,
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            cache_read_tokens: summary.cache_read_tokens,
            cache_write_tokens: summary.cache_write_tokens,
            tool_calls_made: summary.tool_calls_made,
            stop_reason: &summary.stop_reason,
            error: audit_policy::optional_text_digest(summary.error.as_deref()),
        };
        if let Err(err) = append_jsonl(&audit) {
            tracing::error!(error = %err, "failed to write clawd turn audit record");
        }
        HookOutcome::Continue
    }
}

/// Wrap a successful tool call in a session mutation record.
///
/// `mutations.jsonl` is durable, user-readable and replayed by
/// rollback, so the forward payload carries the same allowlisted
/// [`ToolFacts`] the audit log records — never `tool_call.input`, which
/// is model-authored text and has held prompts, queries and file
/// contents.
fn record_tool_mutation(
    ctx: &HookContext,
    tool_call: &ToolCall,
    facts: &ToolFacts,
    result: &ToolResultSummary,
) {
    if ctx.session_id.is_empty() {
        return;
    }
    let Ok(session_id) = ctx.session_id.parse::<SessionId>() else {
        return;
    };
    if !session::session_dir(&session_id).exists() {
        return;
    }

    let record = MutationRecord::new(Mutation::Opaque {
        verb: format!("agent.tool.{}", facts.tool),
        forward: json!({
            "tool": facts.tool,
            "tool_known": facts.known,
            "tool_use_id": audit_policy::safe_identity(&tool_call.id),
            "input": facts.input,
            "input_omitted": facts.input_omitted,
            "turn_index": ctx.turn_index,
        }),
        inverse: json!({
            "unsupported": true,
            "reason": "tool-level transaction wrapper; use typed mutation records when the tool exposes a reversible operation",
            "bytes_returned": result.bytes_returned,
        }),
    })
    .with_runtime("clawd")
    .with_turn(ctx.turn_index as u64);

    if let Err(err) = session::record_mutation(&session_id, record) {
        tracing::warn!(
            error = %err,
            session_id = %session_id.as_str(),
            tool = %facts.tool,
            "failed to record clawd tool mutation wrapper"
        );
    }
}

fn append_jsonl<T: Serialize>(record: &T) -> Result<(), String> {
    let path = crate::paths::data_dir().join("clawd").join("audit.jsonl");
    let line = serde_json::to_string(record).map_err(|err| err.to_string())?;
    crate::filelock::append_locked(&path, &line)
        .map_err(|err| format!("failed to write clawd audit log {}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/audit.rs"
    ));
}
