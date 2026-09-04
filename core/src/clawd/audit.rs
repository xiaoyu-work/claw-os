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
    self, ProtocolFailureFacts, RequestFacts, ResponseFacts, TextDigest, ToolFacts,
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
struct ProtocolFailureAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    #[serde(flatten)]
    request: &'a ProtocolFailureFacts,
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
    schema_version: u32,
    execution_phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_uid: Option<u32>,
    source: &'static str,
    attended: bool,
    local: bool,
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

pub fn record_protocol_failure(
    request: &ProtocolFailureFacts,
    outcome: &ResponseFacts,
    duration: Duration,
    client: &ClientIdentity,
) -> Result<(), String> {
    let audit = ProtocolFailureAudit {
        ts: Utc::now(),
        event: "clawd.protocol-failure",
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
        schema_version: job.schema_version,
        execution_phase: job.execution_phase.as_str(),
        execution_generation: job
            .execution_generation
            .as_deref()
            .map(audit_policy::safe_identity),
        session_id: job.session_id.as_deref().map(audit_policy::safe_identity),
        owner_uid: job.owner_uid,
        source: job.client.source.as_str(),
        attended: job.client.attended,
        local: job.client.local,
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

#[derive(Debug, Serialize)]
struct ExtensionHostSupervisorAudit {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    job_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    owner_uid: u32,
    host_pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    host_start_time_ticks: Option<u64>,
    action: &'static str,
    success: bool,
}

pub fn record_extension_host_event(
    task_id: &str,
    session_id: Option<&str>,
    owner_uid: u32,
    host_pid: u32,
    host_start_time_ticks: Option<u64>,
    action: &'static str,
    success: bool,
) {
    let record = ExtensionHostSupervisorAudit {
        ts: Utc::now(),
        event: "clawd.agent.extension.host",
        job_id: audit_policy::safe_identity(task_id),
        session_id: session_id.map(audit_policy::safe_identity),
        owner_uid,
        host_pid,
        host_start_time_ticks,
        action,
        success,
    };
    if let Err(error) = append_jsonl(&record) {
        tracing::error!(%error, "failed to write extension-host audit record");
    }
    if let Some(session_id) = session_id {
        record_extension_mutation(
            session_id,
            "host",
            action,
            "task-host",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            success,
        );
    }
}

pub fn install_runtime_hook() {
    hooks::global_registry().register(runtime_hook());
}

pub fn runtime_hook() -> Arc<dyn Hook> {
    Arc::new(ClawdRuntimeAuditHook)
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
        Record::ExtensionLifecycle {
            session_id,
            kind,
            action,
            extension_id,
            binding_digest,
            lease_digest,
            stage,
            app,
            mcp,
            abi,
            manifest_digest,
            success,
            latency_ms,
            error,
        } => {
            let record = WorkerExtensionAudit {
                ts: Utc::now(),
                event: "clawd.agent.extension.lifecycle",
                job_id: &job_id,
                owner_uid,
                session_id: audit_policy::safe_identity(session_id),
                kind: kind.as_str(),
                action: action.as_str(),
                extension_id: audit_policy::safe_identity(extension_id),
                binding_digest: audit_policy::safe_reference(binding_digest),
                lease_digest: audit_policy::safe_reference(lease_digest),
                stage: stage.map(|stage| stage.as_str()),
                policy_identity: mcp
                    .as_ref()
                    .map(|mcp| audit_policy::safe_identity(&mcp.policy_identity)),
                server_identity: mcp
                    .as_ref()
                    .map(|mcp| audit_policy::safe_identity(&mcp.server_identity)),
                handle_digest: mcp
                    .as_ref()
                    .map(|mcp| audit_policy::safe_reference(&mcp.handle_digest)),
                descriptor_digest: mcp
                    .as_ref()
                    .map(|mcp| audit_policy::safe_reference(&mcp.descriptor_digest)),
                capability_generation: mcp
                    .as_ref()
                    .map(|mcp| audit_policy::safe_reference(&mcp.capability_generation)),
                untrusted_remote_name: mcp.as_ref().map(|mcp| mcp.untrusted_remote_name.clone()),
                app_tool: app
                    .as_ref()
                    .map(|app| audit_policy::safe_identity(&app.tool)),
                package_digest: abi
                    .as_ref()
                    .map(|abi| audit_policy::safe_reference(&abi.package_digest)),
                event_kind: abi
                    .as_ref()
                    .and_then(|abi| abi.event_kind.map(|kind| kind.as_str())),
                event_id: abi.as_ref().and_then(|abi| abi.event_id.clone()),
                output: abi.as_ref().and_then(|abi| abi.output.clone()),
                action_id: abi.as_ref().and_then(|abi| abi.action_id.clone()),
                tool: abi
                    .as_ref()
                    .and_then(|abi| abi.tool.as_deref().map(audit_policy::safe_identity)),
                capability_ref: abi.as_ref().and_then(|abi| abi.capability_ref.clone()),
                queue_depth: abi.as_ref().and_then(|abi| abi.queue_depth),
                invoke_target: app
                    .as_ref()
                    .map(|app| audit_policy::safe_identity(&app.invoke_target)),
                call_id: app
                    .as_ref()
                    .map(|app| audit_policy::safe_identity(&app.context.call_id)),
                trace_id: app
                    .as_ref()
                    .map(|app| audit_policy::safe_identity(&app.context.trace_id)),
                parent_call_id: app
                    .as_ref()
                    .and_then(|app| app.context.parent_call_id.as_deref())
                    .map(audit_policy::safe_identity),
                call_depth: app.as_ref().map(|app| app.context.depth),
                deadline_unix_ms: app.as_ref().and_then(|app| app.context.deadline_unix_ms),
                caller_kind: app.as_ref().map(|app| app.context.caller.kind.as_str()),
                caller_id: app
                    .as_ref()
                    .map(|app| audit_policy::safe_identity(&app.context.caller.id)),
                caller_app_id: app
                    .as_ref()
                    .and_then(|app| app.context.caller.app_id.as_deref())
                    .map(audit_policy::safe_identity),
                manifest_digest: manifest_digest.as_deref().map(audit_policy::safe_reference),
                success: *success,
                latency_ms: *latency_ms,
                error: error.clone(),
            };
            let result = append_jsonl(&record);
            record_extension_mutation(
                session_id,
                kind.as_str(),
                action.as_str(),
                extension_id,
                Some(binding_digest),
                Some(lease_digest),
                *stage,
                app.as_deref(),
                mcp.as_ref(),
                abi.as_deref(),
                manifest_digest.as_deref(),
                *success,
            );
            result
        }
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
    worker_pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_start_time_ticks: Option<u64>,
    lease: audit_policy::TextDigest,
    session_id: String,
    verb: String,
    scope: String,
    risk: &'static str,
    context: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_digest: Option<String>,
    action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    approval_grant: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    approval_generation: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    authority_grant: Option<&'a crate::clawd::authority::GrantRef>,
}

/// Record one permission mediation the broker performed for a worker.
///
/// Identity comes from the caller's verified lease, and the verb/scope
/// are re-bounded here, so the trail says exactly which consent decision
/// was spent or filed on whose behalf without trusting worker text.
#[allow(clippy::too_many_arguments)]
pub fn record_worker_approval(
    task_id: &str,
    owner_uid: u32,
    worker_pid: u32,
    worker_start_time_ticks: Option<u64>,
    lease_nonce: &str,
    session_id: &str,
    verb: &str,
    scope: &crate::caps::Scope,
    risk: crate::caps::Risk,
    context: crate::caps::ConsentContext,
    operation_digest: Option<&str>,
    action: &'static str,
    approval_grant: Option<&str>,
    approval_generation: Option<u32>,
    authority_grant: Option<&crate::clawd::authority::GrantRef>,
) {
    let job_id = audit_policy::safe_identity(task_id);
    let record = WorkerApprovalAudit {
        ts: Utc::now(),
        event: "clawd.agent.approval.mediated",
        job_id: &job_id,
        owner_uid,
        worker_pid,
        worker_start_time_ticks,
        lease: audit_policy::text_digest(lease_nonce),
        session_id: audit_policy::safe_identity(session_id),
        verb: audit_policy::safe_identity(verb),
        scope: audit_policy::safe_reference(&scope.to_string()),
        risk: risk.as_str(),
        context: context.as_str(),
        operation_digest: operation_digest.map(str::to_string),
        action,
        approval_grant,
        approval_generation,
        authority_grant,
    };
    if let Err(err) = append_jsonl(&record) {
        tracing::error!(error = %err, "failed to write agentd approval audit record");
    }
}

#[derive(Debug, Serialize)]
struct ApprovalRevocationAudit {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    scope: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_uid: Option<u32>,
    /// Digest of the grant session the revocation covers. The session
    /// name is a caller-derived string, so only its keyed digest is
    /// stored; two records about the same session still correlate.
    session: audit_policy::TextDigest,
    /// The generation now in force. Monotonic and non-secret: it says
    /// how many times this scope has been retired, nothing about what
    /// was approved.
    generation: u32,
}

/// Record one reusable-approval revocation.
///
/// Carries no verb, no scope value, no request id and no handle — a
/// revocation is a statement about a counter, and the records it
/// invalidates are already audited by their own decisions.
pub fn record_approval_revocation(
    scope: &crate::approvals::RevocationScope,
    session: &str,
    generation: u32,
) {
    let record = ApprovalRevocationAudit {
        ts: Utc::now(),
        event: "clawd.approval.revoked",
        scope: scope.kind(),
        owner_uid: scope.owner_uid(),
        session: audit_policy::text_digest(session),
        generation,
    };
    if let Err(err) = append_jsonl(&record) {
        tracing::error!(error = %err, "failed to write approval revocation audit record");
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

#[derive(Debug, Serialize)]
struct WorkerExtensionAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    job_id: &'a str,
    owner_uid: u32,
    session_id: String,
    kind: &'static str,
    action: &'static str,
    extension_id: String,
    binding_digest: String,
    lease_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    policy_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    server_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    handle_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    descriptor_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capability_generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    untrusted_remote_name: Option<TextDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event_id: Option<TextDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<TextDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action_id: Option<TextDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capability_ref: Option<TextDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    queue_depth: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invoke_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    call_depth: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deadline_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    caller_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    caller_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    caller_app_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_digest: Option<String>,
    success: bool,
    latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<TextDigest>,
}

fn record_extension_mutation(
    raw_session_id: &str,
    kind: &str,
    action: &str,
    extension_id: &str,
    binding_digest: Option<&str>,
    lease_digest: Option<&str>,
    stage: Option<crate::extension_host::protocol::AuditStage>,
    app: Option<&crate::extension_host::protocol::AppInvocationAudit>,
    mcp: Option<&crate::extension_host::protocol::McpInvocationAudit>,
    abi: Option<&crate::extension_host::protocol::AgentExtensionAudit>,
    manifest_digest: Option<&str>,
    success: bool,
) {
    let Ok(session_id) = raw_session_id.parse::<SessionId>() else {
        return;
    };
    if !session::session_dir(&session_id).exists() {
        return;
    }
    let record = MutationRecord::new(Mutation::Opaque {
        verb: format!("agent.extension.{action}"),
        forward: json!({
            "kind": kind,
            "extension": audit_policy::safe_identity(extension_id),
            "binding_digest": binding_digest.map(audit_policy::safe_reference),
            "lease_digest": lease_digest.map(audit_policy::safe_reference),
            "stage": stage.map(|stage| stage.as_str()),
            "policy_identity": mcp.map(|mcp| audit_policy::safe_identity(&mcp.policy_identity)),
            "server_identity": mcp.map(|mcp| audit_policy::safe_identity(&mcp.server_identity)),
            "handle_digest": mcp.map(|mcp| audit_policy::safe_reference(&mcp.handle_digest)),
            "descriptor_digest": mcp.map(|mcp| audit_policy::safe_reference(&mcp.descriptor_digest)),
            "capability_generation": mcp.map(|mcp| audit_policy::safe_reference(&mcp.capability_generation)),
            "untrusted_remote_name": mcp.map(|mcp| mcp.untrusted_remote_name.clone()),
            "package_digest": abi.map(|abi| audit_policy::safe_reference(&abi.package_digest)),
            "event_kind": abi.and_then(|abi| abi.event_kind.map(|kind| kind.as_str())),
            "event_id": abi.and_then(|abi| abi.event_id.clone()),
            "output": abi.and_then(|abi| abi.output.clone()),
            "action_id": abi.and_then(|abi| abi.action_id.clone()),
            "tool": abi.and_then(|abi| abi.tool.as_deref().map(audit_policy::safe_identity)),
            "capability_ref": abi.and_then(|abi| abi.capability_ref.clone()),
            "queue_depth": abi.and_then(|abi| abi.queue_depth),
            "app_tool": app.map(|app| audit_policy::safe_identity(&app.tool)),
            "invoke_target": app.map(|app| audit_policy::safe_identity(&app.invoke_target)),
            "call_id": app.map(|app| audit_policy::safe_identity(&app.context.call_id)),
            "trace_id": app.map(|app| audit_policy::safe_identity(&app.context.trace_id)),
            "parent_call_id": app
                .and_then(|app| app.context.parent_call_id.as_deref())
                .map(audit_policy::safe_identity),
            "call_depth": app.map(|app| app.context.depth),
            "deadline_unix_ms": app.and_then(|app| app.context.deadline_unix_ms),
            "caller_kind": app.map(|app| app.context.caller.kind.as_str()),
            "caller_id": app.map(|app| audit_policy::safe_identity(&app.context.caller.id)),
            "caller_app_id": app
                .and_then(|app| app.context.caller.app_id.as_deref())
                .map(audit_policy::safe_identity),
            "manifest_digest": manifest_digest.map(audit_policy::safe_reference),
            "success": success,
        }),
        inverse: json!({
            "unsupported": true,
            "reason": "extension lifecycle record"
        }),
    })
    .with_runtime("clawd");
    if let Err(error) = session::record_mutation(&session_id, record) {
        tracing::warn!(
            %error,
            session_id = %session_id.as_str(),
            "failed to record extension lifecycle in session state"
        );
    }
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

pub(crate) fn append_jsonl<T: Serialize>(record: &T) -> Result<(), String> {
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
