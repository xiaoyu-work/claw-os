//! Single agent turn — model call → tool calls → tool results.
//!
//! A "turn" is one round-trip with the LLM. The loop in `loop_.rs` repeats
//! turns until the provider stops requesting tool calls or `max_turns` is
//! reached.

use std::sync::Arc;
use std::time::Instant;

use crate::agent::llm::{
    accumulate::{accumulate_stream, StreamSink},
    run_log::{self, LlmRunRecord},
    ChatRequest, ChatResponse, ContentBlock, FinishReason, Message, Role, Tool as LlmTool,
    ToolCall, ToolChoice, Usage,
};
use crate::agent::runtime::hooks::{self, HookContext};
use crate::agent::runtime::progress::{self, ProgressSink};
use crate::agent::tools::{registry::ToolRegistry, ToolResult};

/// Outcome of one turn.
#[derive(Debug)]
pub enum TurnOutcome {
    /// Provider produced final text and is done.
    Final(String),
    /// Provider asked for tool execution; results have already been appended
    /// to `messages`. Run another turn.
    ContinueWithTools,
}

/// `run_turn` and `run_turn_streaming` return both the
/// [`TurnOutcome`] and the LLM-reported [`Usage`] for the call so
/// callers can plumb token counts into per-turn observability
/// (hook summaries, audit, billing). Usage is zero-filled if the
/// provider doesn't report one.
#[derive(Debug)]
pub struct TurnReport {
    pub outcome: TurnOutcome,
    pub usage: Usage,
}

/// Run a single turn against `provider` with the current `messages`.
///
/// On exit:
/// - `messages` is mutated in place: assistant message + tool result messages
///   (if any) are appended.
/// - Returns whether the loop should continue or terminate.
///
/// `session_id` is included in the per-call run-log record. Pass `None` if
/// memory is disabled / the caller doesn't track sessions.
///
/// `hook_ctx` enables `pre_tool` / `post_tool` dispatch through the
/// global hook registry. Pass `None` to skip hook dispatch entirely
/// (zero-cost for callers that don't care about hooks). Pre/post-turn
/// dispatch is the caller's responsibility.
#[allow(clippy::too_many_arguments)]
pub async fn run_turn(
    provider: Arc<dyn crate::agent::llm::Provider>,
    model: &str,
    system: &str,
    messages: &mut Vec<Message>,
    tools: &ToolRegistry,
    llm_tools: &[LlmTool],
    max_tokens: u32,
    temperature: f32,
    session_id: Option<&str>,
    retry_policy: Option<crate::agent::llm::rate_limit::RetryPolicy>,
    hook_ctx: Option<&HookContext>,
    progress: Arc<dyn ProgressSink>,
) -> Result<TurnReport, super::loop_::AgentError> {
    run_turn_inner(
        provider,
        model,
        system,
        messages,
        tools,
        llm_tools,
        max_tokens,
        temperature,
        session_id,
        retry_policy,
        hook_ctx,
        progress,
        true,
    )
    .await
}

/// Final synthesis turn used when the agent reaches its configured work
/// limit. Tool schemas are removed and `tool_choice` is forced to `none`, so
/// the provider must answer from results already present in `messages`.
#[allow(clippy::too_many_arguments)]
pub async fn run_final_turn(
    provider: Arc<dyn crate::agent::llm::Provider>,
    model: &str,
    system: &str,
    messages: &mut Vec<Message>,
    tools: &ToolRegistry,
    llm_tools: &[LlmTool],
    max_tokens: u32,
    temperature: f32,
    session_id: Option<&str>,
    retry_policy: Option<crate::agent::llm::rate_limit::RetryPolicy>,
    hook_ctx: Option<&HookContext>,
    progress: Arc<dyn ProgressSink>,
) -> Result<TurnReport, super::loop_::AgentError> {
    run_turn_inner(
        provider,
        model,
        system,
        messages,
        tools,
        llm_tools,
        max_tokens,
        temperature,
        session_id,
        retry_policy,
        hook_ctx,
        progress,
        false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_turn_inner(
    provider: Arc<dyn crate::agent::llm::Provider>,
    model: &str,
    system: &str,
    messages: &mut Vec<Message>,
    tools: &ToolRegistry,
    llm_tools: &[LlmTool],
    max_tokens: u32,
    temperature: f32,
    session_id: Option<&str>,
    retry_policy: Option<crate::agent::llm::rate_limit::RetryPolicy>,
    hook_ctx: Option<&HookContext>,
    progress: Arc<dyn ProgressSink>,
    allow_tools: bool,
) -> Result<TurnReport, super::loop_::AgentError> {
    let mut request = ChatRequest {
        model: model.to_string(),
        messages: messages.clone(),
        system: Some(system.to_string()),
        tools: if allow_tools {
            llm_tools.to_vec()
        } else {
            Vec::new()
        },
        tool_choice: if allow_tools {
            ToolChoice::Auto
        } else {
            ToolChoice::None
        },
        max_tokens: Some(max_tokens),
        temperature: Some(temperature),
        top_p: None,
        stop_sequences: vec![],
        extra: serde_json::Value::Null,
    };

    // Prompt-cache markers are no-ops for providers that don't support
    // them (the marker keys live in `request.extra` and are ignored by
    // OpenAI/Gemini/Ollama/llama-local). For Anthropic they translate
    // into `cache_control: {"type":"ephemeral"}` on the system prompt
    // and the last tool definition, which is the recommended cache
    // strategy in `prompt::caching` (system + tools are stable across
    // a session and dominate prompt size, so caching them gives the
    // biggest cost/latency win on every turn after the first).
    if provider.supports_prompt_cache() {
        crate::agent::prompt::caching::mark_system_cached(&mut request);
        if !request.tools.is_empty() {
            crate::agent::prompt::caching::mark_tools_cached(&mut request);
        }
    }

    let start = Instant::now();
    let chat_result = match retry_policy {
        Some(policy) => {
            // Clone the request per attempt: providers may consume
            // owned `extra` fields, and the retry helper needs a
            // fresh future each call.
            let provider_for_retry = provider.clone();
            crate::agent::llm::rate_limit::retry_with_backoff(policy, move || {
                let p = provider_for_retry.clone();
                let req = request.clone();
                async move { p.chat(req).await }
            })
            .await
        }
        None => provider.chat(request).await,
    };
    let duration_ms = start.elapsed().as_millis() as u64;

    // Capture engine_info AFTER the call — for local engines the
    // engine isn't loaded until the first chat() call, so reading
    // before would always return None.
    let engine = provider.engine_info();
    let provider_name = provider.effective_provider_name();
    let effective_model = provider.effective_model_name(model);

    let response = match chat_result {
        Ok(resp) => {
            let rec = LlmRunRecord::from_success(
                &provider_name,
                &effective_model,
                engine,
                resp.finish_reason,
                &resp.usage,
                duration_ms,
                session_id,
            );
            run_log::record(&rec);
            resp
        }
        Err(e) => {
            let rec = LlmRunRecord::from_error(
                &provider_name,
                &effective_model,
                engine,
                &format!("{e}"),
                duration_ms,
                session_id,
            );
            run_log::record(&rec);
            return Err(super::loop_::AgentError::Llm(e));
        }
    };

    // Always append the assistant message verbatim so subsequent turns have
    // the full history.
    messages.push(Message {
        role: Role::Assistant,
        content: response.content.clone(),
    });

    // Capture usage up-front so we can plumb it into TurnReport on
    // every exit path; the response is consumed below by
    // tool-call collection and finish-reason matching.
    let usage = response.usage.clone();

    let tool_calls = collect_tool_calls(&response);

    if tool_calls.is_empty() || !allow_tools {
        let text = extract_text(&response);
        return Ok(TurnReport {
            outcome: TurnOutcome::Final(text),
            usage,
        });
    }

    // Dispatch each tool call. Results are appended as a single user-role
    // message with one ToolResult block per call (Anthropic convention; other
    // providers' adapters can re-shape on the way out).
    //
    // When `hook_ctx` is Some, every dispatch goes through pre_tool /
    // post_tool — Deny short-circuits with a synthetic error result;
    // Override substitutes the input. A post_tool Stop is captured but
    // does NOT short-circuit mid-loop: we let the remaining tools run
    // and append the full results message before propagating
    // Interrupted, so message history (assistant tool_use ↔ user
    // tool_result) stays balanced for the next turn.
    //
    // Tools that opt into [`Tool::parallel_safe`] run concurrently
    // via `dispatch_calls`; others serialise. See that helper for the
    // ordering guarantees.
    let (result_blocks, pending_stop) =
        dispatch_calls(tools, hook_ctx, &tool_calls, session_id, progress.as_ref()).await;
    messages.push(Message {
        role: Role::User,
        content: result_blocks,
    });

    if let Some(reason) = pending_stop {
        return Err(super::loop_::AgentError::Interrupted(format!(
            "hook stop (post_tool): {reason}"
        )));
    }

    // If the provider explicitly said Stop despite producing tool_use blocks,
    // honour that — but typical providers signal `ToolUse` here.
    let outcome = match response.finish_reason {
        FinishReason::Stop
        | FinishReason::Length
        | FinishReason::Refusal
        | FinishReason::ContentFilter
        | FinishReason::Other => {
            // Loop will terminate even though tools ran — the provider has
            // declared it's done.
            TurnOutcome::Final(extract_text(&response))
        }
        FinishReason::ToolUse => TurnOutcome::ContinueWithTools,
    };
    Ok(TurnReport { outcome, usage })
}

/// Streaming variant of [`run_turn`] that drives the provider via
/// [`crate::agent::llm::Provider::chat_stream`] and forwards every
/// streamed event to `sink` before assembling them back into a
/// `ChatResponse`. The rest of the turn logic (tool dispatch,
/// message append, finish-reason handling) is identical to
/// [`run_turn`].
///
/// Live-token UIs (TUI, websocket, SSE-to-client) plug their
/// `StreamSink` here. `retry_policy` is intentionally unused —
/// streaming retries would require the provider to surface the
/// retryable error before any bytes flow, which the current
/// `chat_stream` contract doesn't guarantee.
#[allow(clippy::too_many_arguments)]
pub async fn run_turn_streaming(
    provider: Arc<dyn crate::agent::llm::Provider>,
    model: &str,
    system: &str,
    messages: &mut Vec<Message>,
    tools: &ToolRegistry,
    llm_tools: &[LlmTool],
    max_tokens: u32,
    temperature: f32,
    session_id: Option<&str>,
    sink: Arc<dyn StreamSink>,
    hook_ctx: Option<&HookContext>,
    progress: Arc<dyn ProgressSink>,
) -> Result<TurnReport, super::loop_::AgentError> {
    run_turn_streaming_inner(
        provider,
        model,
        system,
        messages,
        tools,
        llm_tools,
        max_tokens,
        temperature,
        session_id,
        sink,
        hook_ctx,
        progress,
        true,
    )
    .await
}

/// Streaming counterpart of [`run_final_turn`].
#[allow(clippy::too_many_arguments)]
pub async fn run_final_turn_streaming(
    provider: Arc<dyn crate::agent::llm::Provider>,
    model: &str,
    system: &str,
    messages: &mut Vec<Message>,
    tools: &ToolRegistry,
    llm_tools: &[LlmTool],
    max_tokens: u32,
    temperature: f32,
    session_id: Option<&str>,
    sink: Arc<dyn StreamSink>,
    hook_ctx: Option<&HookContext>,
    progress: Arc<dyn ProgressSink>,
) -> Result<TurnReport, super::loop_::AgentError> {
    run_turn_streaming_inner(
        provider,
        model,
        system,
        messages,
        tools,
        llm_tools,
        max_tokens,
        temperature,
        session_id,
        sink,
        hook_ctx,
        progress,
        false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_turn_streaming_inner(
    provider: Arc<dyn crate::agent::llm::Provider>,
    model: &str,
    system: &str,
    messages: &mut Vec<Message>,
    tools: &ToolRegistry,
    llm_tools: &[LlmTool],
    max_tokens: u32,
    temperature: f32,
    session_id: Option<&str>,
    sink: Arc<dyn StreamSink>,
    hook_ctx: Option<&HookContext>,
    progress: Arc<dyn ProgressSink>,
    allow_tools: bool,
) -> Result<TurnReport, super::loop_::AgentError> {
    let mut request = ChatRequest {
        model: model.to_string(),
        messages: messages.clone(),
        system: Some(system.to_string()),
        tools: if allow_tools {
            llm_tools.to_vec()
        } else {
            Vec::new()
        },
        tool_choice: if allow_tools {
            ToolChoice::Auto
        } else {
            ToolChoice::None
        },
        max_tokens: Some(max_tokens),
        temperature: Some(temperature),
        top_p: None,
        stop_sequences: vec![],
        extra: serde_json::Value::Null,
    };

    if provider.supports_prompt_cache() {
        crate::agent::prompt::caching::mark_system_cached(&mut request);
        if !request.tools.is_empty() {
            crate::agent::prompt::caching::mark_tools_cached(&mut request);
        }
    }

    let start = Instant::now();
    let stream_result = provider.chat_stream(request).await;
    let chat_result: crate::agent::llm::Result<ChatResponse> = match stream_result {
        Ok(stream) => accumulate_stream(stream, sink.clone(), model).await,
        Err(e) => Err(e),
    };
    let duration_ms = start.elapsed().as_millis() as u64;

    let engine = provider.engine_info();
    let provider_name = provider.effective_provider_name();
    let effective_model = provider.effective_model_name(model);

    let response = match chat_result {
        Ok(resp) => {
            let rec = LlmRunRecord::from_success(
                &provider_name,
                &effective_model,
                engine,
                resp.finish_reason,
                &resp.usage,
                duration_ms,
                session_id,
            );
            run_log::record(&rec);
            resp
        }
        Err(e) => {
            let rec = LlmRunRecord::from_error(
                &provider_name,
                &effective_model,
                engine,
                &format!("{e}"),
                duration_ms,
                session_id,
            );
            run_log::record(&rec);
            return Err(super::loop_::AgentError::Llm(e));
        }
    };

    messages.push(Message {
        role: Role::Assistant,
        content: response.content.clone(),
    });

    let usage = response.usage.clone();
    let tool_calls = collect_tool_calls(&response);

    if tool_calls.is_empty() || !allow_tools {
        let text = extract_text(&response);
        return Ok(TurnReport {
            outcome: TurnOutcome::Final(text),
            usage,
        });
    }

    let (result_blocks, pending_stop) =
        dispatch_calls(tools, hook_ctx, &tool_calls, session_id, progress.as_ref()).await;
    messages.push(Message {
        role: Role::User,
        content: result_blocks,
    });

    if let Some(reason) = pending_stop {
        return Err(super::loop_::AgentError::Interrupted(format!(
            "hook stop (post_tool): {reason}"
        )));
    }

    let outcome = match response.finish_reason {
        FinishReason::Stop
        | FinishReason::Length
        | FinishReason::Refusal
        | FinishReason::ContentFilter
        | FinishReason::Other => TurnOutcome::Final(extract_text(&response)),
        FinishReason::ToolUse => TurnOutcome::ContinueWithTools,
    };
    Ok(TurnReport { outcome, usage })
}

fn collect_tool_calls(response: &ChatResponse) -> Vec<ToolCall> {
    if !response.tool_calls.is_empty() {
        return response.tool_calls.clone();
    }
    // Fallback: pull tool_use out of content blocks if the provider didn't
    // populate the convenience field.
    response
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some(ToolCall {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn extract_text(response: &ChatResponse) -> String {
    let mut out = String::new();
    for b in &response.content {
        if let ContentBlock::Text { text } = b {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    out
}

/// Inject the runtime's real `session_id` into session-scoped tools so
/// the model can't write to the wrong session file. `cos_todo` persists
/// per-session under `<data>/agent/todos/<session_id>.json`; when the
/// model omits the id the tool defaults to `"default"` and silently
/// writes to the wrong list (see `tools::todo`). When the runtime knows
/// the session id we override whatever the model supplied.
fn effective_tool_input(call: &ToolCall, session_id: Option<&str>) -> serde_json::Value {
    const SESSION_SCOPED_TOOLS: &[&str] = &["cos_todo"];
    match session_id {
        Some(sid) if !sid.is_empty() && SESSION_SCOPED_TOOLS.contains(&call.name.as_str()) => {
            let mut input = call.input.clone();
            if let serde_json::Value::Object(ref mut map) = input {
                map.insert(
                    "session_id".to_string(),
                    serde_json::Value::String(sid.to_string()),
                );
            }
            input
        }
        _ => call.input.clone(),
    }
}

async fn dispatch_tool(
    registry: &ToolRegistry,
    call: &ToolCall,
    session_id: Option<&str>,
) -> ToolResult {
    // Per-call approval gate. Skip the await entirely when the tool
    // is not configured under any of the three sets. `is_classified`
    // is one O(1) HashSet lookup that covers
    // `auto_approve ∪ auto_deny ∪ dangerous` — vs. three
    // separate `BTreeSet::contains` calls on the hot path.
    let approval = registry.approval();
    if approval.is_classified(&call.name) {
        let outcome = approval
            .evaluate(&call.name, &call.input, "policy: dangerous_tools")
            .await;
        match outcome {
            crate::agent::runtime::approval::ApprovalOutcome::Approved { .. } => {
                // fall through to tool dispatch
            }
            crate::agent::runtime::approval::ApprovalOutcome::Denied { reason } => {
                return ToolResult::err(format!(
                    "approval denied for `{}`: {}",
                    call.name,
                    reason.unwrap_or_else(|| "no reason".to_string())
                ));
            }
            crate::agent::runtime::approval::ApprovalOutcome::Deferred { prompt } => {
                // Headless / non-interactive deferral. Surfaces back
                // to the model as an error tool_result so it can ask
                // the user (or pick a different approach). The runtime
                // does NOT block — that would deadlock under
                // `ask_blocking` and there's nowhere to send a prompt.
                return ToolResult::err(format!(
                    "approval pending for `{}`: {}",
                    call.name,
                    prompt.unwrap_or_else(|| "user approval required".to_string())
                ));
            }
        }
    }

    match registry.get(&call.name) {
        Some(tool) => {
            // Scope the task-locals that `cos_delegate` (and any other
            // policy-aware tool) reads to discover the parent registry's
            // current guardrails + approval gate. Without this scope a
            // child agent spawned via `cos_delegate` would run under
            // permissive defaults regardless of the parent's deny rules
            // or approval policy — a privilege-escalation bug at the
            // delegate boundary.
            let g = registry.guardrails().clone();
            let a = registry.approval().clone();
            crate::agent::tools::delegate::PARENT_GUARDRAILS
                .scope(
                    g,
                    crate::agent::tools::delegate::PARENT_APPROVAL
                        .scope(a, tool.exec(effective_tool_input(call, session_id))),
                )
                .await
        }
        None => ToolResult::err(format!(
            "unknown tool '{}'. registered: {:?}",
            call.name,
            registry.names()
        )),
    }
}

/// Outcome of a single dispatch: the (possibly-overridden) call, the
/// tool result, and the wall-clock latency we measured. Returned by
/// [`dispatch_one`] so the caller can run post-hooks and assemble
/// `ContentBlock::ToolResult` after waiting on a batch of futures.
struct DispatchOutcome {
    effective_call: ToolCall,
    result: ToolResult,
    latency_ms: u64,
}

/// Resolve pre-hook for `call`. Returns the effective call (with
/// `Override` applied) and an optional synthetic error string when
/// the hook denied. The returned values mirror what the old serial
/// loop computed inline.
fn apply_pre_hook(hook_ctx: Option<&HookContext>, call: &ToolCall) -> (ToolCall, Option<String>) {
    match hook_ctx {
        Some(ctx) => {
            let registry = hooks::global_registry();
            match registry.dispatch_pre_tool(ctx, call) {
                hooks::ToolDecision::Allow => (call.clone(), None),
                hooks::ToolDecision::Deny(reason) => (
                    call.clone(),
                    Some(format!("hook deny `{}`: {reason}", call.name)),
                ),
                hooks::ToolDecision::Override(new_input) => {
                    let mut overridden = call.clone();
                    overridden.input = new_input;
                    (overridden, None)
                }
            }
        }
        None => (call.clone(), None),
    }
}

/// Run a single tool call end-to-end: pre-hook, [`ProgressSink::on_tool_start`],
/// dispatch (or deny short-circuit), latency, [`ProgressSink::on_tool_result`].
/// Does NOT run the post-hook — that's the caller's job, sequentially
/// across the assembled outcomes, so a parallel batch can still
/// produce a deterministic `pending_stop` order.
async fn dispatch_one(
    tools: &ToolRegistry,
    hook_ctx: Option<&HookContext>,
    call: &ToolCall,
    session_id: Option<&str>,
    progress: &dyn ProgressSink,
) -> DispatchOutcome {
    let (effective_call, decision_error) = apply_pre_hook(hook_ctx, call);

    progress.on_tool_start(&effective_call.id, &effective_call.name, &effective_call.input);

    let started = Instant::now();
    let result = if let Some(reason) = decision_error {
        ToolResult::err(reason)
    } else {
        dispatch_tool(tools, &effective_call, session_id).await
    };
    let latency_ms = started.elapsed().as_millis() as u64;

    let ok = !result.is_error;
    let bytes = result.content.len();
    let preview = progress::render_preview(&result.content, ok);
    progress.on_tool_result(
        &effective_call.id,
        &effective_call.name,
        ok,
        latency_ms,
        bytes,
        &preview,
    );

    DispatchOutcome {
        effective_call,
        result,
        latency_ms,
    }
}

/// Dispatch every tool call in `tool_calls` and return `(result_blocks,
/// pending_stop)`. Calls that opt into [`Tool::parallel_safe`] run
/// concurrently via `futures_util::future::join_all`; the rest serialise.
///
/// Ordering guarantees:
/// * `result_blocks` is returned in the original `tool_calls` order
///   (the LLM contract requires tool_result blocks to match tool_use
///   blocks 1:1 in order).
/// * Post-hooks run sequentially in the original order after every
///   dispatch completes, so `pending_stop` reflects the first hook
///   that signalled Stop in declaration order — same semantics as
///   the old serial loop.
async fn dispatch_calls(
    tools: &ToolRegistry,
    hook_ctx: Option<&HookContext>,
    tool_calls: &[ToolCall],
    session_id: Option<&str>,
    progress: &dyn ProgressSink,
) -> (Vec<ContentBlock>, Option<String>) {
    // Partition into parallel-safe vs serial groups, preserving the
    // original index so we can interleave the results back in order.
    let mut parallel: Vec<usize> = Vec::new();
    let mut serial: Vec<usize> = Vec::new();
    for (i, call) in tool_calls.iter().enumerate() {
        if tools.is_parallel_safe(&call.name) {
            parallel.push(i);
        } else {
            serial.push(i);
        }
    }

    // Pre-size with `None` so we can place results by index regardless
    // of completion order. `take`/`unwrap` at the end converts to
    // `Vec<DispatchOutcome>` in declaration order.
    let mut slots: Vec<Option<DispatchOutcome>> = (0..tool_calls.len()).map(|_| None).collect();

    // Concurrent batch first. `join_all` polls every future in this
    // task; no `spawn` is needed and no `Send` bound creeps in.
    if !parallel.is_empty() {
        let futs = parallel.iter().map(|&i| {
            let call = &tool_calls[i];
            async move {
                let outcome = dispatch_one(tools, hook_ctx, call, session_id, progress).await;
                (i, outcome)
            }
        });
        let results = futures_util::future::join_all(futs).await;
        for (i, outcome) in results {
            slots[i] = Some(outcome);
        }
    }

    // Serial group runs after the parallel batch finished. This is a
    // deliberate ordering — concurrent inspection calls (sysinfo,
    // proxy reads) settle first; side-effecting calls (shell exec,
    // fs writes) then run with the latest state. Matches the
    // expected mental model: "do the safe reads, then the writes".
    for i in serial {
        let outcome = dispatch_one(tools, hook_ctx, &tool_calls[i], session_id, progress).await;
        slots[i] = Some(outcome);
    }

    // Assemble result blocks + run post-hooks in declaration order.
    let mut result_blocks: Vec<ContentBlock> = Vec::with_capacity(tool_calls.len());
    let mut pending_stop: Option<String> = None;
    for (i, slot) in slots.into_iter().enumerate() {
        let outcome = slot.expect("every dispatch slot must be filled");
        if let Some(ctx) = hook_ctx {
            let summary = hooks::ToolResultSummary {
                tool_name: outcome.effective_call.name.clone(),
                success: !outcome.result.is_error,
                latency_ms: outcome.latency_ms,
                bytes_returned: outcome.result.content.len(),
                error: if outcome.result.is_error {
                    Some(outcome.result.content.clone())
                } else {
                    None
                },
            };
            if pending_stop.is_none() {
                if let hooks::HookOutcome::Stop(reason) = hooks::global_registry()
                    .dispatch_post_tool(ctx, &outcome.effective_call, &summary)
                {
                    pending_stop = Some(reason);
                }
            }
        }
        // tool_use_id must match the original LLM-issued id, not the
        // effective (overridden) one — providers correlate by the id
        // they assigned in the assistant message.
        result_blocks.push(ContentBlock::ToolResult {
            tool_use_id: tool_calls[i].id.clone(),
            is_error: outcome.result.is_error,
            content: outcome.result.content,
        });
    }
    (result_blocks, pending_stop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    use crate::agent::llm::ToolCall;
    use crate::agent::runtime::hooks::{
        global_registry, Hook, HookContext, HookOutcome, ToolDecision, ToolResultSummary,
    };
    use crate::agent::tools::registry::builtin_only_registry;
    use crate::config::AgentConfig;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn cfg() -> AgentConfig {
        AgentConfig {
            provider: "mock".into(),
            model: "mock-model".into(),
            max_turns: 5,
            max_tokens: 1024,
            temperature: 0.0,
            system_prompt_path: None,
            ..Default::default()
        }
    }

    fn ctx() -> HookContext {
        HookContext::new("turn-tests".to_string(), "mock", "mock-model".to_string())
    }

    /// pre_tool returning Allow runs the tool unmodified and the
    /// hook's post_tool sees a successful result summary.
    #[tokio::test]
    async fn pre_tool_allow_passes_through() {
        struct Spy {
            pre_calls: Arc<AtomicU32>,
            post_success: Arc<AtomicU32>,
        }
        impl Hook for Spy {
            fn name(&self) -> &str {
                "turn-allow-spy"
            }
            fn pre_tool(&self, _c: &HookContext, _t: &ToolCall) -> ToolDecision {
                self.pre_calls.fetch_add(1, Ordering::SeqCst);
                ToolDecision::Allow
            }
            fn post_tool(
                &self,
                _c: &HookContext,
                _t: &ToolCall,
                s: &ToolResultSummary,
            ) -> HookOutcome {
                if s.success {
                    self.post_success.fetch_add(1, Ordering::SeqCst);
                }
                HookOutcome::Continue
            }
        }
        let pre = Arc::new(AtomicU32::new(0));
        let post = Arc::new(AtomicU32::new(0));
        global_registry().register(Arc::new(Spy {
            pre_calls: pre.clone(),
            post_success: post.clone(),
        }));

        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "c1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "hi"}),
        }]));
        mock.push_response(MockResponse::Text("done".into()));
        let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let llm_tools = tools.as_llm_tools();
        let mut messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "go".into() }],
        }];
        let hctx = ctx();

        let _ = run_turn(
            provider.clone(),
            &cfg.model,
            "sys",
            &mut messages,
            &tools,
            &llm_tools,
            cfg.max_tokens,
            cfg.temperature,
            None,
            None,
            Some(&hctx),
            progress::null_progress(),
        )
        .await
        .unwrap();

        global_registry().unregister("turn-allow-spy");

        assert_eq!(pre.load(Ordering::SeqCst), 1);
        assert_eq!(post.load(Ordering::SeqCst), 1);
    }

    /// pre_tool returning Deny short-circuits the dispatch and feeds
    /// the deny reason back as a `tool_result` error block. The real
    /// tool is never invoked.
    #[tokio::test]
    async fn pre_tool_deny_short_circuits_dispatch() {
        struct Denier;
        impl Hook for Denier {
            fn name(&self) -> &str {
                "turn-denier"
            }
            fn pre_tool(&self, _c: &HookContext, _t: &ToolCall) -> ToolDecision {
                ToolDecision::Deny("policy: blocked-in-test".into())
            }
        }
        global_registry().register(Arc::new(Denier));

        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "c1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "hi"}),
        }]));
        mock.push_response(MockResponse::Text("done".into()));
        let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let llm_tools = tools.as_llm_tools();
        let mut messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "go".into() }],
        }];
        let hctx = ctx();

        let _ = run_turn(
            provider.clone(),
            &cfg.model,
            "sys",
            &mut messages,
            &tools,
            &llm_tools,
            cfg.max_tokens,
            cfg.temperature,
            None,
            None,
            Some(&hctx),
            progress::null_progress(),
        )
        .await
        .unwrap();

        global_registry().unregister("turn-denier");

        // The tool_result message is the last one (User role with ToolResult blocks).
        let last = messages.last().unwrap();
        assert_eq!(last.role, Role::User);
        let block = last.content.first().unwrap();
        match block {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(*is_error, "tool_result should be an error");
                assert!(content.contains("hook deny"), "got {content}");
                assert!(content.contains("policy: blocked-in-test"), "got {content}");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    /// pre_tool returning Override substitutes the tool input. Echo
    /// tool returns the substituted text, proving the original input
    /// was replaced.
    #[tokio::test]
    async fn pre_tool_override_substitutes_input() {
        struct Overrider;
        impl Hook for Overrider {
            fn name(&self) -> &str {
                "turn-overrider"
            }
            fn pre_tool(&self, _c: &HookContext, _t: &ToolCall) -> ToolDecision {
                ToolDecision::Override(serde_json::json!({"text": "REPLACED"}))
            }
        }
        global_registry().register(Arc::new(Overrider));

        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "c1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "ORIGINAL"}),
        }]));
        mock.push_response(MockResponse::Text("done".into()));
        let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let llm_tools = tools.as_llm_tools();
        let mut messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "go".into() }],
        }];
        let hctx = ctx();

        let _ = run_turn(
            provider.clone(),
            &cfg.model,
            "sys",
            &mut messages,
            &tools,
            &llm_tools,
            cfg.max_tokens,
            cfg.temperature,
            None,
            None,
            Some(&hctx),
            progress::null_progress(),
        )
        .await
        .unwrap();

        global_registry().unregister("turn-overrider");

        let last = messages.last().unwrap();
        let block = last.content.first().unwrap();
        match block {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(!*is_error);
                assert!(content.contains("REPLACED"), "got {content}");
                assert!(!content.contains("ORIGINAL"), "got {content}");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    /// post_tool returning Stop captures a Stop reason but lets the
    /// loop finish appending all tool_results before propagating
    /// AgentError::Interrupted. This keeps assistant tool_use ↔ user
    /// tool_result history balanced.
    #[tokio::test]
    async fn post_tool_stop_propagates_after_results_appended() {
        struct Stopper;
        impl Hook for Stopper {
            fn name(&self) -> &str {
                "turn-post-stopper"
            }
            fn post_tool(
                &self,
                _c: &HookContext,
                _t: &ToolCall,
                _s: &ToolResultSummary,
            ) -> HookOutcome {
                HookOutcome::Stop("audit-veto".into())
            }
        }
        global_registry().register(Arc::new(Stopper));

        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::ToolUse(vec![
            ToolCall {
                id: "c1".into(),
                name: "echo".into(),
                input: serde_json::json!({"text": "first"}),
            },
            ToolCall {
                id: "c2".into(),
                name: "echo".into(),
                input: serde_json::json!({"text": "second"}),
            },
        ]));
        let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let llm_tools = tools.as_llm_tools();
        let mut messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "go".into() }],
        }];
        let hctx = ctx();

        let result = run_turn(
            provider.clone(),
            &cfg.model,
            "sys",
            &mut messages,
            &tools,
            &llm_tools,
            cfg.max_tokens,
            cfg.temperature,
            None,
            None,
            Some(&hctx),
            progress::null_progress(),
        )
        .await;

        global_registry().unregister("turn-post-stopper");

        match result {
            Err(super::super::loop_::AgentError::Interrupted(reason)) => {
                assert!(reason.contains("audit-veto"), "got {reason}");
                assert!(reason.contains("post_tool"), "got {reason}");
            }
            other => panic!("expected Interrupted, got {other:?}"),
        }
        // History is balanced: assistant message (with two tool_use)
        // and a user message with two tool_result blocks both got
        // appended before the Interrupted bubbled up.
        assert_eq!(messages.len(), 3); // initial user + assistant + tool-results user
        let last = messages.last().unwrap();
        assert_eq!(last.content.len(), 2, "both tool_results appended");
    }

    /// hook_ctx = None disables all hook dispatch — proves the
    /// zero-cost path for callers that don't care about hooks. We
    /// register a hook that would Deny if it ran, then verify the
    /// real tool ran (not denied) because dispatch was skipped.
    #[tokio::test]
    async fn hook_ctx_none_skips_dispatch_entirely() {
        struct WouldDeny;
        impl Hook for WouldDeny {
            fn name(&self) -> &str {
                "turn-would-deny"
            }
            fn pre_tool(&self, _c: &HookContext, _t: &ToolCall) -> ToolDecision {
                ToolDecision::Deny("should not run".into())
            }
        }
        global_registry().register(Arc::new(WouldDeny));

        let cfg = cfg();
        let mock = MockProvider::new(&cfg.model, &cfg);
        mock.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "c1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "ORIGINAL"}),
        }]));
        mock.push_response(MockResponse::Text("done".into()));
        let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(mock);
        let tools = builtin_only_registry();
        let llm_tools = tools.as_llm_tools();
        let mut messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "go".into() }],
        }];

        let _ = run_turn(
            provider.clone(),
            &cfg.model,
            "sys",
            &mut messages,
            &tools,
            &llm_tools,
            cfg.max_tokens,
            cfg.temperature,
            None,
            None,
            None, // <— no hook context: dispatch skipped
            progress::null_progress(),
        )
        .await
        .unwrap();

        global_registry().unregister("turn-would-deny");

        let last = messages.last().unwrap();
        let block = last.content.first().unwrap();
        match block {
            ContentBlock::ToolResult {
                is_error, content, ..
            } => {
                assert!(!*is_error, "deny should not have fired");
                assert!(content.contains("ORIGINAL"), "got {content}");
                assert!(!content.contains("should not run"), "got {content}");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    // ----------------------------------------------------------------
    // ProgressSink + parallel-dispatch unit tests
    // ----------------------------------------------------------------

    use crate::agent::tools::{Tool, ToolResult as TR};

    /// Recording sink: every callback captures (id, name, ok?, latency).
    /// Used to assert the runtime called us at the right moments with
    /// the right arguments.
    #[derive(Default)]
    struct RecordingProgress {
        starts: std::sync::Mutex<Vec<(String, String)>>,
        results: std::sync::Mutex<Vec<(String, String, bool, usize)>>,
    }
    impl progress::ProgressSink for RecordingProgress {
        fn on_tool_start(&self, id: &str, name: &str, _input: &serde_json::Value) {
            self.starts
                .lock()
                .unwrap()
                .push((id.to_string(), name.to_string()));
        }
        fn on_tool_result(
            &self,
            id: &str,
            name: &str,
            ok: bool,
            _latency_ms: u64,
            bytes_returned: usize,
            _preview: &str,
        ) {
            self.results.lock().unwrap().push((
                id.to_string(),
                name.to_string(),
                ok,
                bytes_returned,
            ));
        }
    }

    /// Slow read-only tool. Sleeps for `delay` then returns. Marked
    /// `parallel_safe = true` so the dispatch loop runs siblings
    /// concurrently. Used to verify the parallel batch actually
    /// overlaps work in wall time.
    struct SlowReader {
        name: &'static str,
        delay: std::time::Duration,
    }
    #[async_trait::async_trait]
    impl Tool for SlowReader {
        fn name(&self) -> &'static str {
            self.name
        }
        fn description(&self) -> &'static str {
            "slow read"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type":"object"})
        }
        async fn exec(&self, _input: serde_json::Value) -> TR {
            tokio::time::sleep(self.delay).await;
            TR::ok(format!("done {}", self.name))
        }
        fn parallel_safe(&self) -> bool {
            true
        }
    }

    /// Side-effecting tool. Default `parallel_safe = false`.
    struct SerialWriter {
        name: &'static str,
        delay: std::time::Duration,
    }
    #[async_trait::async_trait]
    impl Tool for SerialWriter {
        fn name(&self) -> &'static str {
            self.name
        }
        fn description(&self) -> &'static str {
            "serial write"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type":"object"})
        }
        async fn exec(&self, _input: serde_json::Value) -> TR {
            tokio::time::sleep(self.delay).await;
            TR::ok(format!("wrote {}", self.name))
        }
        // parallel_safe = false (default)
    }

    fn registry_with(tools_vec: Vec<Arc<dyn Tool>>) -> ToolRegistry {
        let mut r = ToolRegistry::new();
        for t in tools_vec {
            r.register(t);
        }
        r
    }

    fn calls(specs: &[(&str, &str)]) -> Vec<ToolCall> {
        specs
            .iter()
            .map(|(id, name)| ToolCall {
                id: (*id).to_string(),
                name: (*name).to_string(),
                input: serde_json::json!({}),
            })
            .collect()
    }

    /// Progress sink receives exactly one start + one result per
    /// dispatched tool call, in declaration order for the serial
    /// path.
    #[tokio::test]
    async fn progress_sink_fires_for_every_dispatch_in_order() {
        let registry = registry_with(vec![Arc::new(SerialWriter {
            name: "w1",
            delay: std::time::Duration::from_millis(1),
        })]);
        let tool_calls = calls(&[("id-1", "w1"), ("id-2", "w1")]);
        let p = Arc::new(RecordingProgress::default());
        let (blocks, stop) =
            dispatch_calls(&registry, None, &tool_calls, None, p.as_ref() as &dyn progress::ProgressSink)
                .await;
        assert!(stop.is_none());
        assert_eq!(blocks.len(), 2);
        let starts = p.starts.lock().unwrap();
        let results = p.results.lock().unwrap();
        assert_eq!(
            starts.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["id-1", "id-2"]
        );
        assert_eq!(
            results
                .iter()
                .map(|(id, _, ok, _)| (id.as_str(), *ok))
                .collect::<Vec<_>>(),
            vec![("id-1", true), ("id-2", true)]
        );
    }

    /// Parallel-safe tools dispatched concurrently complete in
    /// max(durations) rather than sum(durations). Three 100ms
    /// sleeps must finish in well under 300ms.
    #[tokio::test]
    async fn parallel_safe_tools_dispatch_concurrently() {
        let delay = std::time::Duration::from_millis(100);
        let registry = registry_with(vec![
            Arc::new(SlowReader { name: "r1", delay }),
            Arc::new(SlowReader { name: "r2", delay }),
            Arc::new(SlowReader { name: "r3", delay }),
        ]);
        let tool_calls = calls(&[("a", "r1"), ("b", "r2"), ("c", "r3")]);
        let p = progress::null_progress();
        let started = std::time::Instant::now();
        let (blocks, _) =
            dispatch_calls(&registry, None, &tool_calls, None, p.as_ref() as &dyn progress::ProgressSink)
                .await;
        let elapsed = started.elapsed();
        assert_eq!(blocks.len(), 3);
        // Sequential dispatch would take ~300ms. Concurrent
        // dispatch finishes inside one delay plus scheduling
        // slack; 250ms is a generous upper bound that still proves
        // overlap occurred.
        assert!(
            elapsed < std::time::Duration::from_millis(250),
            "expected concurrent dispatch, got {elapsed:?}"
        );
    }

    /// `parallel_safe = false` tools serialise even when batched
    /// together. Three 80ms serial writers must take at least
    /// 240ms total.
    #[tokio::test]
    async fn serial_tools_remain_sequential() {
        let delay = std::time::Duration::from_millis(80);
        let registry = registry_with(vec![
            Arc::new(SerialWriter { name: "w1", delay }),
            Arc::new(SerialWriter { name: "w2", delay }),
            Arc::new(SerialWriter { name: "w3", delay }),
        ]);
        let tool_calls = calls(&[("a", "w1"), ("b", "w2"), ("c", "w3")]);
        let p = progress::null_progress();
        let started = std::time::Instant::now();
        let _ =
            dispatch_calls(&registry, None, &tool_calls, None, p.as_ref() as &dyn progress::ProgressSink)
                .await;
        let elapsed = started.elapsed();
        assert!(
            elapsed >= std::time::Duration::from_millis(220),
            "expected serial dispatch (~240ms), got {elapsed:?}"
        );
    }

    /// Mixed batch: result_blocks are returned in original
    /// declaration order even when readers (parallel) finish
    /// before writers (serial).
    #[tokio::test]
    async fn mixed_batch_preserves_declaration_order() {
        let fast = std::time::Duration::from_millis(10);
        let slow = std::time::Duration::from_millis(50);
        let registry = registry_with(vec![
            Arc::new(SerialWriter {
                name: "w1",
                delay: slow,
            }),
            Arc::new(SlowReader {
                name: "r1",
                delay: fast,
            }),
            Arc::new(SerialWriter {
                name: "w2",
                delay: slow,
            }),
        ]);
        // Declaration order: w1 (serial), r1 (parallel), w2 (serial).
        let tool_calls = calls(&[("id-w1", "w1"), ("id-r1", "r1"), ("id-w2", "w2")]);
        let p = progress::null_progress();
        let (blocks, _) =
            dispatch_calls(&registry, None, &tool_calls, None, p.as_ref() as &dyn progress::ProgressSink)
                .await;
        let ids: Vec<&str> = blocks
            .iter()
            .map(|b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => tool_use_id.as_str(),
                _ => panic!("expected ToolResult"),
            })
            .collect();
        assert_eq!(ids, vec!["id-w1", "id-r1", "id-w2"]);
    }
}
