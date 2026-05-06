//! Single agent turn — model call → tool calls → tool results.
//!
//! A "turn" is one round-trip with the LLM. The loop in `loop_.rs` repeats
//! turns until the provider stops requesting tool calls or `max_turns` is
//! reached.

use std::sync::Arc;
use std::time::Instant;

use crate::agent::llm::{
    run_log::{self, LlmRunRecord},
    ChatRequest, ChatResponse, ContentBlock, FinishReason, Message, Role, Tool as LlmTool,
    ToolChoice, ToolCall,
};
use crate::agent::tools::{registry::ToolRegistry, ToolResult};

/// Outcome of one turn.
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
    let mut result_blocks: Vec<ContentBlock> = Vec::with_capacity(tool_calls.len());
    for call in &tool_calls {
        let result = dispatch_tool(tools, call).await;
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
