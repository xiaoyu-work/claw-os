//! Single agent turn — model call → tool calls → tool results.
//!
//! A "turn" is one round-trip with the LLM. The loop in `loop_.rs` repeats
//! turns until the provider stops requesting tool calls or `max_turns` is
//! reached.

use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use crate::agent::llm::{
    accumulate::{accumulate_stream, StreamSink},
    run_log::{self, LlmRunRecord},
    ChatRequest, ChatResponse, ContentBlock, FinishReason, Message, Role, Tool as LlmTool,
    ToolCall, ToolChoice, Usage,
};
use crate::agent::runtime::hooks::{self, HookContext};
use crate::agent::runtime::interrupt;
use crate::agent::runtime::progress::{self, ProgressSink};
use crate::agent::tools::exposure::ToolExposureContext;
use crate::agent::tools::registry::{ResolvedToolCall, ResolvedToolKind, ToolRegistry};
use crate::agent::tools::ToolResult;

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

/// Provider delivery is the only per-mode variation in a turn. Request
/// construction, run logging, message transitions, tool dispatch, and outcome
/// selection are shared below in [`run_turn_inner`].
enum ProviderDelivery {
    Buffered {
        retry_policy: Option<crate::agent::llm::rate_limit::RetryPolicy>,
    },
    Streaming {
        sink: Arc<dyn StreamSink>,
    },
}

struct TurnRequest<'a> {
    provider: Arc<dyn crate::agent::llm::Provider>,
    model: &'a str,
    system: &'a str,
    messages: &'a mut Vec<Message>,
    tools: &'a ToolRegistry,
    exposure: Option<&'a ToolExposureContext>,
    llm_tools: &'a [LlmTool],
    max_tokens: u32,
    temperature: f32,
    session_id: Option<&'a str>,
    hook_ctx: Option<&'a HookContext>,
    progress: Arc<dyn ProgressSink>,
    allow_tools: bool,
    delivery: ProviderDelivery,
    interrupt: Option<&'a interrupt::Handle>,
}

fn interrupted(handle: &interrupt::Handle) -> super::loop_::AgentError {
    super::loop_::AgentError::Interrupted(handle.session_id().to_string())
}

fn check_interrupted(handle: Option<&interrupt::Handle>) -> Result<(), super::loop_::AgentError> {
    match handle {
        Some(handle) if handle.check() => Err(interrupted(handle)),
        _ => Ok(()),
    }
}

async fn await_interruptible<F>(
    handle: Option<&interrupt::Handle>,
    future: F,
) -> Result<F::Output, super::loop_::AgentError>
where
    F: Future,
{
    match handle {
        Some(handle) => {
            tokio::select! {
                biased;
                _ = handle.cancelled() => Err(interrupted(handle)),
                output = future => Ok(output),
            }
        }
        None => Ok(future.await),
    }
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
/// `hook_ctx` enables `pre_tool` / `post_tool` dispatch through its
/// explicitly composed hook registry. Pass `None` to skip hook dispatch entirely
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
    run_turn_inner(TurnRequest {
        provider,
        model,
        system,
        messages,
        tools,
        exposure: None,
        llm_tools,
        max_tokens,
        temperature,
        session_id,
        hook_ctx,
        progress,
        allow_tools: true,
        delivery: ProviderDelivery::Buffered { retry_policy },
        interrupt: None,
    })
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_turn_interruptible(
    provider: Arc<dyn crate::agent::llm::Provider>,
    model: &str,
    system: &str,
    messages: &mut Vec<Message>,
    tools: &ToolRegistry,
    exposure: &ToolExposureContext,
    llm_tools: &[LlmTool],
    max_tokens: u32,
    temperature: f32,
    session_id: Option<&str>,
    retry_policy: Option<crate::agent::llm::rate_limit::RetryPolicy>,
    hook_ctx: Option<&HookContext>,
    progress: Arc<dyn ProgressSink>,
    interrupt: &interrupt::Handle,
) -> Result<TurnReport, super::loop_::AgentError> {
    run_turn_inner(TurnRequest {
        provider,
        model,
        system,
        messages,
        tools,
        exposure: Some(exposure),
        llm_tools,
        max_tokens,
        temperature,
        session_id,
        hook_ctx,
        progress,
        allow_tools: true,
        delivery: ProviderDelivery::Buffered { retry_policy },
        interrupt: Some(interrupt),
    })
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
    run_turn_inner(TurnRequest {
        provider,
        model,
        system,
        messages,
        tools,
        exposure: None,
        llm_tools,
        max_tokens,
        temperature,
        session_id,
        hook_ctx,
        progress,
        allow_tools: false,
        delivery: ProviderDelivery::Buffered { retry_policy },
        interrupt: None,
    })
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_final_turn_interruptible(
    provider: Arc<dyn crate::agent::llm::Provider>,
    model: &str,
    system: &str,
    messages: &mut Vec<Message>,
    tools: &ToolRegistry,
    exposure: &ToolExposureContext,
    llm_tools: &[LlmTool],
    max_tokens: u32,
    temperature: f32,
    session_id: Option<&str>,
    retry_policy: Option<crate::agent::llm::rate_limit::RetryPolicy>,
    hook_ctx: Option<&HookContext>,
    progress: Arc<dyn ProgressSink>,
    interrupt: &interrupt::Handle,
) -> Result<TurnReport, super::loop_::AgentError> {
    run_turn_inner(TurnRequest {
        provider,
        model,
        system,
        messages,
        tools,
        exposure: Some(exposure),
        llm_tools,
        max_tokens,
        temperature,
        session_id,
        hook_ctx,
        progress,
        allow_tools: false,
        delivery: ProviderDelivery::Buffered { retry_policy },
        interrupt: Some(interrupt),
    })
    .await
}

async fn run_turn_inner(request: TurnRequest<'_>) -> Result<TurnReport, super::loop_::AgentError> {
    let TurnRequest {
        provider,
        model,
        system,
        messages,
        tools,
        exposure,
        llm_tools,
        max_tokens,
        temperature,
        session_id,
        hook_ctx,
        progress,
        allow_tools,
        delivery,
        interrupt,
    } = request;
    let fallback_exposure = ToolExposureContext::isolated(tools.guardrails().clone());
    let exposure = exposure.unwrap_or(&fallback_exposure);
    check_interrupted(interrupt)?;
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
    let chat_result = await_interruptible(interrupt, async {
        match delivery {
            ProviderDelivery::Buffered { retry_policy } => match retry_policy {
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
            },
            ProviderDelivery::Streaming { sink } => match provider.chat_stream(request).await {
                Ok(stream) => accumulate_stream(stream, sink, model).await,
                Err(error) => Err(error),
            },
        }
    })
    .await?;
    check_interrupted(interrupt)?;
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
    let (result_blocks, pending_stop) = dispatch_calls(
        tools,
        exposure,
        hook_ctx,
        &tool_calls,
        session_id,
        progress.as_ref(),
        interrupt,
    )
    .await?;
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
        None,
        llm_tools,
        max_tokens,
        temperature,
        session_id,
        sink,
        hook_ctx,
        progress,
        true,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_turn_streaming_interruptible(
    provider: Arc<dyn crate::agent::llm::Provider>,
    model: &str,
    system: &str,
    messages: &mut Vec<Message>,
    tools: &ToolRegistry,
    exposure: &ToolExposureContext,
    llm_tools: &[LlmTool],
    max_tokens: u32,
    temperature: f32,
    session_id: Option<&str>,
    sink: Arc<dyn StreamSink>,
    hook_ctx: Option<&HookContext>,
    progress: Arc<dyn ProgressSink>,
    interrupt: &interrupt::Handle,
) -> Result<TurnReport, super::loop_::AgentError> {
    run_turn_streaming_inner(
        provider,
        model,
        system,
        messages,
        tools,
        Some(exposure),
        llm_tools,
        max_tokens,
        temperature,
        session_id,
        sink,
        hook_ctx,
        progress,
        true,
        Some(interrupt),
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
        None,
        llm_tools,
        max_tokens,
        temperature,
        session_id,
        sink,
        hook_ctx,
        progress,
        false,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_final_turn_streaming_interruptible(
    provider: Arc<dyn crate::agent::llm::Provider>,
    model: &str,
    system: &str,
    messages: &mut Vec<Message>,
    tools: &ToolRegistry,
    exposure: &ToolExposureContext,
    llm_tools: &[LlmTool],
    max_tokens: u32,
    temperature: f32,
    session_id: Option<&str>,
    sink: Arc<dyn StreamSink>,
    hook_ctx: Option<&HookContext>,
    progress: Arc<dyn ProgressSink>,
    interrupt: &interrupt::Handle,
) -> Result<TurnReport, super::loop_::AgentError> {
    run_turn_streaming_inner(
        provider,
        model,
        system,
        messages,
        tools,
        Some(exposure),
        llm_tools,
        max_tokens,
        temperature,
        session_id,
        sink,
        hook_ctx,
        progress,
        false,
        Some(interrupt),
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
    exposure: Option<&ToolExposureContext>,
    llm_tools: &[LlmTool],
    max_tokens: u32,
    temperature: f32,
    session_id: Option<&str>,
    sink: Arc<dyn StreamSink>,
    hook_ctx: Option<&HookContext>,
    progress: Arc<dyn ProgressSink>,
    allow_tools: bool,
    interrupt: Option<&interrupt::Handle>,
) -> Result<TurnReport, super::loop_::AgentError> {
    run_turn_inner(TurnRequest {
        provider,
        model,
        system,
        messages,
        tools,
        exposure,
        llm_tools,
        max_tokens,
        temperature,
        session_id,
        hook_ctx,
        progress,
        allow_tools,
        delivery: ProviderDelivery::Streaming { sink },
        interrupt,
    })
    .await
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
pub(crate) fn effective_tool_input(
    call: &ToolCall,
    session_id: Option<&str>,
    exposure: &ToolExposureContext,
) -> serde_json::Value {
    const SESSION_SCOPED_TOOLS: &[&str] = &["cos_todo"];
    let session_id = session_id
        .filter(|value| !value.is_empty())
        .or_else(|| exposure.conversation_session_id())
        .or_else(|| {
            (!exposure.authority_session_id().is_empty()).then(|| exposure.authority_session_id())
        });
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
    exposure: &ToolExposureContext,
    kind: &ResolvedToolKind,
    call: &ToolCall,
    session_id: Option<&str>,
) -> ToolResult {
    match kind {
        ResolvedToolKind::Registry => {
            registry
                .execute(
                    exposure,
                    &call.name,
                    effective_tool_input(call, session_id, exposure),
                    "policy: dangerous_tools",
                )
                .await
        }
        ResolvedToolKind::Catalog => registry.execute_catalog(exposure, &call.name, &call.input),
        ResolvedToolKind::Rejected(reason) => ToolResult::err(reason.clone()),
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
        Some(ctx) => match hooks::current_registry().dispatch_pre_tool(ctx, call) {
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
        },
        None => (call.clone(), None),
    }
}

async fn wait_progress_ready(
    progress: &dyn ProgressSink,
    interrupt: Option<&interrupt::Handle>,
) -> Result<(), super::loop_::AgentError> {
    if let Some(ready) = progress.wait_ready() {
        let ready = await_interruptible(interrupt, ready).await?;
        check_interrupted(interrupt)?;
        if !ready {
            return Err(super::loop_::AgentError::Internal(
                "tool progress sink closed".into(),
            ));
        }
    }
    Ok(())
}

/// Run a single tool call end-to-end: pre-hook, [`ProgressSink::on_tool_start`],
/// dispatch (or deny short-circuit), latency, [`ProgressSink::on_tool_result`].
/// Does NOT run the post-hook — that's the caller's job, sequentially
/// across the assembled outcomes, so a parallel batch can still
/// produce a deterministic `pending_stop` order.
async fn dispatch_one(
    tools: &ToolRegistry,
    exposure: &ToolExposureContext,
    hook_ctx: Option<&HookContext>,
    invocation: &ResolvedToolCall,
    session_id: Option<&str>,
    progress: &dyn ProgressSink,
    interrupt: Option<&interrupt::Handle>,
) -> Result<DispatchOutcome, super::loop_::AgentError> {
    check_interrupted(interrupt)?;
    let (effective_call, hook_error) = apply_pre_hook(hook_ctx, &invocation.call);
    let decision_error = match (&invocation.kind, hook_error) {
        (ResolvedToolKind::Rejected(reason), _) => Some(reason.clone()),
        (_, hook_error) => hook_error,
    };

    wait_progress_ready(progress, interrupt).await?;
    progress.on_tool_start(
        &effective_call.id,
        &effective_call.name,
        &effective_call.input,
    );
    check_interrupted(interrupt)?;

    let started = Instant::now();
    let result = if let Some(reason) = decision_error {
        ToolResult::err(reason)
    } else {
        await_interruptible(
            interrupt,
            dispatch_tool(
                tools,
                exposure,
                &invocation.kind,
                &effective_call,
                session_id,
            ),
        )
        .await?
    };
    let latency_ms = started.elapsed().as_millis() as u64;

    let ok = !result.is_error;
    let bytes = result.content.len();
    let preview = progress::render_preview(&result.content, ok);
    wait_progress_ready(progress, interrupt).await?;
    progress.on_tool_result(
        &effective_call.id,
        &effective_call.name,
        ok,
        latency_ms,
        bytes,
        &preview,
    );
    check_interrupted(interrupt)?;

    Ok(DispatchOutcome {
        effective_call,
        result,
        latency_ms,
    })
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
    exposure: &ToolExposureContext,
    hook_ctx: Option<&HookContext>,
    tool_calls: &[ToolCall],
    session_id: Option<&str>,
    progress: &dyn ProgressSink,
    interrupt: Option<&interrupt::Handle>,
) -> Result<(Vec<ContentBlock>, Option<String>), super::loop_::AgentError> {
    check_interrupted(interrupt)?;
    let resolved = tool_calls
        .iter()
        .map(|call| tools.resolve_model_call(exposure, call))
        .collect::<Vec<_>>();

    // Partition into parallel-safe vs serial groups, preserving the
    // original index so we can interleave the results back in order.
    let mut parallel: Vec<usize> = Vec::new();
    let mut serial: Vec<usize> = Vec::new();
    for (i, invocation) in resolved.iter().enumerate() {
        if tools.is_parallel_safe_resolved(exposure, invocation) {
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
            let invocation = &resolved[i];
            async move {
                let outcome = dispatch_one(
                    tools, exposure, hook_ctx, invocation, session_id, progress, interrupt,
                )
                .await?;
                Ok::<_, super::loop_::AgentError>((i, outcome))
            }
        });
        let results = futures_util::future::join_all(futs).await;
        for result in results {
            let (i, outcome) = result?;
            slots[i] = Some(outcome);
        }
    }

    // Serial group runs after the parallel batch finished. This is a
    // deliberate ordering — concurrent inspection calls (sysinfo,
    // proxy reads) settle first; side-effecting calls (shell exec,
    // fs writes) then run with the latest state. Matches the
    // expected mental model: "do the safe reads, then the writes".
    for i in serial {
        let outcome = dispatch_one(
            tools,
            exposure,
            hook_ctx,
            &resolved[i],
            session_id,
            progress,
            interrupt,
        )
        .await?;
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
                result_digest: crate::crypto::sha256_hex(outcome.result.content.as_bytes()),
                error: if outcome.result.is_error {
                    Some(outcome.result.content.clone())
                } else {
                    None
                },
            };
            if pending_stop.is_none() {
                if let hooks::HookOutcome::Stop(reason) = hooks::current_registry()
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
            content: label_tool_result(&outcome.effective_call.name, outcome.result.content),
        });
    }
    Ok((result_blocks, pending_stop))
}

/// Label one tool result before it re-enters model context.
///
/// Tool output is the classic injection carrier: the model asked for
/// it, but a third party wrote it. Three rules apply, in order:
///
/// 1. A result its own adapter already fenced (MCP, Skill disclosure,
///    memory recall, the progressive bridge) is left alone — fencing
///    twice only costs tokens.
/// 2. Otherwise the tool's *identity* — decided by the registry before
///    the model call, and not attacker-chosen — selects a
///    [`SourceKind`] through [`SourceKind::for_tool_result`].
/// 3. The body is fenced under that label. The fallback,
///    [`SourceKind::BuiltinToolResult`], is still untrusted: a kernel
///    primitive faithfully reports process names, file contents and
///    network responses a third party may control.
///
/// This changes what the model *reads*. It changes nothing about what
/// may *run*: the registry, guardrails and the capability authority
/// already decided which tools exist and which calls were allowed
/// before this function is reached, and none of them consults a label.
fn label_tool_result(tool_name: &str, content: String) -> String {
    use crate::agent::trust::{envelope, SourceKind};

    if envelope::looks_enveloped(&content) {
        return content;
    }
    let kind = SourceKind::for_tool_result(tool_name);
    crate::agent::safety::untrusted::wrap_labeled(kind, Some(tool_name), &content)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/runtime/turn.rs"
    ));
}
