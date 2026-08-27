use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};

use crate::agent::llm::ToolCall;
use crate::agent::runtime::hooks::{
    self, Hook, HookContext, HookOutcome, ToolDecision, ToolResultSummary, TurnSummary,
};
use crate::session::{self, Mutation, MutationRecord, SessionId};

use super::client_identity::ClientIdentity;
use super::protocol::Response;

#[derive(Debug, Serialize)]
struct RequestAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    command: &'a str,
    ok: bool,
    duration_ms: u128,
    params: &'a Value,
    client: &'a ClientIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_message: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct InvalidRequestAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    ok: bool,
    duration_ms: u128,
    raw: &'a str,
    client: &'a ClientIdentity,
    error_code: &'a str,
    error_message: &'a str,
}

#[derive(Debug, Serialize)]
struct TaskAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    job_id: &'a str,
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_start_time_ticks: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct RuntimeTurnAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    session_id: &'a str,
    turn_index: u32,
    provider: &'a str,
    model: &'a str,
    success: bool,
    latency_ms: u64,
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
    tool_calls_made: u32,
    stop_reason: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct RuntimeToolAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    session_id: &'a str,
    turn_index: u32,
    tool_name: &'a str,
    tool_use_id: &'a str,
    success: bool,
    latency_ms: u64,
    bytes_returned: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

pub fn record_request(
    command: &str,
    params: &Value,
    response: &Response,
    duration: Duration,
    client: &ClientIdentity,
) -> Result<(), String> {
    let redacted = redact_params(params);
    let audit = RequestAudit {
        ts: Utc::now(),
        event: "clawd.request",
        command,
        ok: response.ok,
        duration_ms: duration.as_millis(),
        params: redacted.as_ref().unwrap_or(params),
        client,
        error_code: response.error.as_ref().map(|err| err.code.as_str()),
        error_message: response.error.as_ref().map(|err| err.message.as_str()),
    };
    append_jsonl(&audit)
}

/// Request fields that are bearer authority rather than description.
/// The App-session launch handle authorises binding, re-scoping and
/// tearing down a registered session, so it must not be replayable from
/// any record of the request.
const REDACTED_PARAM_KEYS: &[&str] = &["handle"];

/// Return a copy of `params` with bearer fields masked, or `None` when
/// there is nothing to mask so the common path stays allocation-free.
///
/// Shared with [`super::system_journal::record_clawd_request`] so both
/// sinks that persist raw broker requests mask exactly the same fields.
pub(crate) fn redact_params(params: &Value) -> Option<Value> {
    let Value::Object(map) = params else {
        return None;
    };
    if !REDACTED_PARAM_KEYS.iter().any(|key| map.contains_key(*key)) {
        return None;
    }
    let mut redacted = map.clone();
    for key in REDACTED_PARAM_KEYS {
        if let Some(value) = redacted.get_mut(*key) {
            *value = Value::String("<redacted>".to_string());
        }
    }
    Some(Value::Object(redacted))
}

pub fn record_invalid(
    raw: &str,
    response: &Response,
    duration: Duration,
    client: &ClientIdentity,
) -> Result<(), String> {
    let (error_code, error_message) = response
        .error
        .as_ref()
        .map(|err| (err.code.as_str(), err.message.as_str()))
        .unwrap_or(("invalid_json", "invalid JSON request"));
    let audit = InvalidRequestAudit {
        ts: Utc::now(),
        event: "clawd.invalid-request",
        ok: response.ok,
        duration_ms: duration.as_millis(),
        raw,
        client,
        error_code,
        error_message,
    };
    append_jsonl(&audit)
}

pub fn record_task_event(event: &'static str, job: &crate::agent::service::Job) {
    let audit = TaskAudit {
        ts: Utc::now(),
        event,
        job_id: &job.id,
        status: job.status.as_str(),
        session_id: job.session_id.as_deref(),
        worker_pid: job.worker_pid,
        worker_start_time_ticks: job.worker_start_time_ticks,
        provider: job.provider.as_deref(),
        model: job.model.as_deref(),
        error: job.error.as_deref(),
    };
    if let Err(err) = append_jsonl(&audit) {
        tracing::error!(error = %err, event, job_id = %job.id, "failed to write clawd task audit record");
    }
    super::system_journal::record_task_event(event, job);
}

pub fn install_runtime_hook() {
    hooks::global_registry().register(Arc::new(ClawdRuntimeAuditHook));
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
            session_id: &ctx.session_id,
            turn_index: ctx.turn_index,
            tool_name: &tool_call.name,
            tool_use_id: &tool_call.id,
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
        let audit = RuntimeToolAudit {
            ts: Utc::now(),
            event: "clawd.agent.tool.finished",
            session_id: &ctx.session_id,
            turn_index: ctx.turn_index,
            tool_name: &tool_call.name,
            tool_use_id: &tool_call.id,
            success: result.success,
            latency_ms: result.latency_ms,
            bytes_returned: result.bytes_returned,
            error: result.error.as_deref(),
        };
        if let Err(err) = append_jsonl(&audit) {
            tracing::error!(error = %err, "failed to write clawd post-tool audit record");
        }
        if result.success {
            record_tool_mutation(ctx, tool_call, result);
        }
        HookOutcome::Continue
    }

    fn post_turn(&self, ctx: &HookContext, summary: &TurnSummary) -> HookOutcome {
        let audit = RuntimeTurnAudit {
            ts: Utc::now(),
            event: "clawd.agent.turn.finished",
            session_id: &ctx.session_id,
            turn_index: ctx.turn_index,
            provider: &ctx.provider,
            model: &ctx.model,
            success: summary.success,
            latency_ms: summary.latency_ms,
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            cache_read_tokens: summary.cache_read_tokens,
            cache_write_tokens: summary.cache_write_tokens,
            tool_calls_made: summary.tool_calls_made,
            stop_reason: &summary.stop_reason,
            error: summary.error.as_deref(),
        };
        if let Err(err) = append_jsonl(&audit) {
            tracing::error!(error = %err, "failed to write clawd turn audit record");
        }
        HookOutcome::Continue
    }
}

fn record_tool_mutation(ctx: &HookContext, tool_call: &ToolCall, result: &ToolResultSummary) {
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
        verb: format!("agent.tool.{}", tool_call.name),
        forward: json!({
            "tool": tool_call.name,
            "tool_use_id": tool_call.id,
            "input": tool_call.input,
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
            tool = %tool_call.name,
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
