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
    ToolChoice, ToolCall,
};
use crate::agent::runtime::hooks::{self, HookContext};
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
) -> Result<TurnOutcome, super::loop_::AgentError> {
    let mut request = ChatRequest {
        model: model.to_string(),
        messages: messages.clone(),
        system: Some(system.to_string()),
        tools: llm_tools.to_vec(),
        tool_choice: ToolChoice::Auto,
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
    let provider_name = provider.name().to_string();

    let response = match chat_result {
        Ok(resp) => {
            let rec = LlmRunRecord::from_success(
                &provider_name,
                model,
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
                model,
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

    let tool_calls = collect_tool_calls(&response);

    if tool_calls.is_empty() {
        let text = extract_text(&response);
        return Ok(TurnOutcome::Final(text));
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
    let mut result_blocks: Vec<ContentBlock> = Vec::with_capacity(tool_calls.len());
    let mut pending_stop: Option<String> = None;
    for call in &tool_calls {
        let (effective_call, decision_error) = match hook_ctx {
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
        };

        let started = Instant::now();
        let result = if let Some(reason) = decision_error {
            ToolResult::err(reason)
        } else {
            dispatch_tool(tools, &effective_call).await
        };
        let latency_ms = started.elapsed().as_millis() as u64;

        if let Some(ctx) = hook_ctx {
            let summary = hooks::ToolResultSummary {
                tool_name: effective_call.name.clone(),
                success: !result.is_error,
                latency_ms,
                bytes_returned: result.content.len(),
                error: if result.is_error { Some(result.content.clone()) } else { None },
            };
            if pending_stop.is_none() {
                if let hooks::HookOutcome::Stop(reason) = hooks::global_registry()
                    .dispatch_post_tool(ctx, &effective_call, &summary)
                {
                    pending_stop = Some(reason);
                }
            }
        }

        result_blocks.push(ContentBlock::ToolResult {
            tool_use_id: call.id.clone(),
            is_error: result.is_error,
            content: result.content,
        });
    }
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
    match response.finish_reason {
        FinishReason::Stop | FinishReason::Length | FinishReason::Refusal | FinishReason::ContentFilter | FinishReason::Other => {
            // Loop will terminate even though tools ran — the provider has
            // declared it's done.
            Ok(TurnOutcome::Final(extract_text(&response)))
        }
        FinishReason::ToolUse => Ok(TurnOutcome::ContinueWithTools),
    }
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
) -> Result<TurnOutcome, super::loop_::AgentError> {
    let mut request = ChatRequest {
        model: model.to_string(),
        messages: messages.clone(),
        system: Some(system.to_string()),
        tools: llm_tools.to_vec(),
        tool_choice: ToolChoice::Auto,
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
    let provider_name = provider.name().to_string();

    let response = match chat_result {
        Ok(resp) => {
            let rec = LlmRunRecord::from_success(
                &provider_name,
                model,
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
                model,
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

    let tool_calls = collect_tool_calls(&response);

    if tool_calls.is_empty() {
        let text = extract_text(&response);
        return Ok(TurnOutcome::Final(text));
    }

    let mut result_blocks: Vec<ContentBlock> = Vec::with_capacity(tool_calls.len());
    let mut pending_stop: Option<String> = None;
    for call in &tool_calls {
        let (effective_call, decision_error) = match hook_ctx {
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
        };

        let started = Instant::now();
        let result = if let Some(reason) = decision_error {
            ToolResult::err(reason)
        } else {
            dispatch_tool(tools, &effective_call).await
        };
        let latency_ms = started.elapsed().as_millis() as u64;

        if let Some(ctx) = hook_ctx {
            let summary = hooks::ToolResultSummary {
                tool_name: effective_call.name.clone(),
                success: !result.is_error,
                latency_ms,
                bytes_returned: result.content.len(),
                error: if result.is_error { Some(result.content.clone()) } else { None },
            };
            if pending_stop.is_none() {
                if let hooks::HookOutcome::Stop(reason) = hooks::global_registry()
                    .dispatch_post_tool(ctx, &effective_call, &summary)
                {
                    pending_stop = Some(reason);
                }
            }
        }

        result_blocks.push(ContentBlock::ToolResult {
            tool_use_id: call.id.clone(),
            is_error: result.is_error,
            content: result.content,
        });
    }
    messages.push(Message {
        role: Role::User,
        content: result_blocks,
    });

    if let Some(reason) = pending_stop {
        return Err(super::loop_::AgentError::Interrupted(format!(
            "hook stop (post_tool): {reason}"
        )));
    }

    match response.finish_reason {
        FinishReason::Stop
        | FinishReason::Length
        | FinishReason::Refusal
        | FinishReason::ContentFilter
        | FinishReason::Other => Ok(TurnOutcome::Final(extract_text(&response))),
        FinishReason::ToolUse => Ok(TurnOutcome::ContinueWithTools),
    }
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

async fn dispatch_tool(registry: &ToolRegistry, call: &ToolCall) -> ToolResult {
    // Per-call approval gate. Skip the await entirely when the tool
    // is not configured under any of the three sets — the default
    // ApprovalGate has all sets empty, so this is a one-set-contains
    // check per dispatch in the common case.
    let cfg = registry.approval().config();
    if cfg.auto_deny.contains(&call.name)
        || cfg.auto_approve.contains(&call.name)
        || cfg.dangerous.contains(&call.name)
    {
        let outcome = registry
            .approval()
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
        Some(tool) => tool.exec(call.input.clone()).await,
        None => ToolResult::err(format!(
            "unknown tool '{}'. registered: {:?}",
            call.name,
            registry.names()
        )),
    }
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
}

