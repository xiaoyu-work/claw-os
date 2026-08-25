//! Owns Chat Completions request serialization, response parsing, and SSE normalization.

use futures_util::stream::{BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::agent::llm::{
    ChatRequest, ChatResponse, ContentBlock, FinishReason, LlmError, Result, Role, StreamEvent,
    Tool, ToolCall, ToolChoice, Usage,
};

// --- Request --------------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct OutgoingMessage<'a> {
    pub role: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OutgoingToolCall<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OutgoingToolCall<'a> {
    pub id: &'a str,
    #[serde(rename = "type")]
    pub type_: &'static str,
    pub function: OutgoingFunctionCall<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OutgoingFunctionCall<'a> {
    pub name: &'a str,
    pub arguments: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct OutgoingTool<'a> {
    #[serde(rename = "type")]
    pub type_: &'static str,
    pub function: OutgoingFunctionDef<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OutgoingFunctionDef<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub parameters: &'a serde_json::Value,
}

/// Newer OpenAI models (o-series, gpt-5+) reject `max_tokens` and
/// require `max_completion_tokens`. Older models (gpt-4o, gpt-4.1,
/// claude-via-openrouter, deepseek, ollama) still expect
/// `max_tokens`. Heuristic by model name prefix.
pub(crate) fn use_max_completion_tokens(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    // Strip Azure deployment suffixes / variants like "gpt-5.4-mini"
    // → still starts with "gpt-5". Match on the family prefix.
    m.starts_with("gpt-5")
        || m.starts_with("gpt-6")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
}

/// Build the JSON body for `POST /v1/chat/completions`. Pure — no IO.
pub(crate) fn build_request_body(
    request: &ChatRequest,
    model: &str,
    stream: bool,
) -> serde_json::Value {
    let mut messages: Vec<serde_json::Value> = Vec::with_capacity(request.messages.len() + 1);
    if let Some(sys) = &request.system {
        messages.push(serde_json::json!({ "role": "system", "content": sys }));
    }
    for m in &request.messages {
        for v in message_to_json_many(m) {
            messages.push(v);
        }
    }

    let tools: Vec<serde_json::Value> = request.tools.iter().map(tool_to_json).collect();

    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
    });

    let modern = use_max_completion_tokens(model);

    if let Some(obj) = body.as_object_mut() {
        if !tools.is_empty() {
            obj.insert("tools".into(), serde_json::Value::Array(tools));
            obj.insert(
                "tool_choice".into(),
                tool_choice_to_json(&request.tool_choice),
            );
        }
        if let Some(v) = request.max_tokens {
            let key = if modern {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            obj.insert(key.into(), serde_json::json!(v));
        }
        if let Some(v) = request.temperature {
            // o-series / gpt-5 only support the default temperature
            // (1.0). Sending any other value yields a 400. Skip the
            // field entirely for those models.
            if !modern {
                obj.insert("temperature".into(), serde_json::json!(v));
            }
        }
        if let Some(v) = request.top_p {
            if !modern {
                obj.insert("top_p".into(), serde_json::json!(v));
            }
        }
        if !request.stop_sequences.is_empty() {
            obj.insert("stop".into(), serde_json::json!(request.stop_sequences));
        }
        if stream {
            obj.insert("stream".into(), serde_json::json!(true));
        }
        // Merge provider-specific extras (e.g. `seed`, `response_format`).
        if let serde_json::Value::Object(extra) = &request.extra {
            for (k, v) in extra {
                if k.starts_with("_cos_") {
                    continue;
                }
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    body
}

fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn message_to_json(m: &crate::agent::llm::Message) -> serde_json::Value {
    // Back-compat single-output wrapper for tests that pre-date
    // the multi-tool-result fan-out. Returns the first emitted
    // wire message; the request path uses `message_to_json_many`
    // directly so multi-result messages are handled correctly.
    let mut all = message_to_json_many(m);
    if all.is_empty() {
        serde_json::json!({"role": role_to_str(m.role), "content": ""})
    } else {
        all.remove(0)
    }
}

/// Translate one runtime Message into one or more OpenAI-style
/// wire messages.
///
/// OpenAI's schema requires that each tool result is its own
/// message with `role=tool` + `tool_call_id`. The runtime
/// aggregates all tool results for a given assistant turn into a
/// single `User` message holding `Vec<ContentBlock::ToolResult>`
/// (see `runtime/turn.rs`). Translating that to a single wire
/// message would silently drop the second+ ToolResult, leaving
/// the conversation history malformed (assistant.tool_calls with
/// no matching tool messages) — which Azure rejects with a 400.
///
/// We fan out: a User message that consists *only* of
/// ToolResult blocks becomes N separate `role=tool` messages
/// preserving their order. All other messages map 1:1.
fn message_to_json_many(m: &crate::agent::llm::Message) -> Vec<serde_json::Value> {
    let role = role_to_str(m.role);

    // Multi-tool-result fan-out: any message whose blocks are
    // *all* ToolResult is split into N tool messages.
    if !m.content.is_empty()
        && m.content
            .iter()
            .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
    {
        return m
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => Some(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": content,
                })),
                _ => None,
            })
            .collect();
    }

    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    for block in &m.content {
        match block {
            ContentBlock::Text { text } => text_parts.push(text.clone()),
            ContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(serde_json::json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
                    }
                }));
            }
            ContentBlock::ToolResult { .. } => {
                // Pure-tool-result messages were fanned out above.
                // A mixed message containing a ToolResult would
                // be malformed input; we drop the result here
                // rather than silently emit a broken user
                // message. (Should not happen with the current
                // runtime.)
            }
            ContentBlock::Reasoning { .. } => {
                // Responses-only provider state must not leak into the
                // Chat Completions wire format.
            }
            ContentBlock::ToolState { .. } => {
                // Copilot-specific function-call metadata is only valid
                // on the Responses wire format.
            }
            ContentBlock::Image { media_type, data } => {
                text_parts.push(format!("[image {} base64 attached]", media_type));
                let _ = data;
            }
        }
    }

    let mut obj = serde_json::Map::new();
    obj.insert("role".into(), serde_json::json!(role));
    if !text_parts.is_empty() {
        obj.insert("content".into(), serde_json::json!(text_parts.join("\n")));
    } else if tool_calls.is_empty() {
        obj.insert("content".into(), serde_json::json!(""));
    }
    if !tool_calls.is_empty() {
        obj.insert("tool_calls".into(), serde_json::Value::Array(tool_calls));
    }
    vec![serde_json::Value::Object(obj)]
}

fn tool_to_json(t: &Tool) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": t.input_schema,
        }
    })
}

fn tool_choice_to_json(c: &ToolChoice) -> serde_json::Value {
    match c {
        ToolChoice::Auto => serde_json::json!("auto"),
        ToolChoice::None => serde_json::json!("none"),
        ToolChoice::Required => serde_json::json!("required"),
        ToolChoice::Tool { name } => serde_json::json!({
            "type": "function",
            "function": { "name": name }
        }),
    }
}

// --- Response -------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct Response {
    #[serde(default)]
    pub model: Option<String>,
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<UsageJson>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Choice {
    pub message: ChoiceMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChoiceMessage {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<IncomingToolCall>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IncomingToolCall {
    pub id: String,
    #[serde(default)]
    pub function: IncomingFunctionCall,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct IncomingFunctionCall {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UsageJson {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
}

pub(crate) fn response_to_chat(resp: Response, fallback_model: &str) -> Result<ChatResponse> {
    let choice = resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| LlmError::Parse("response had no choices".into()))?;

    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    if let Some(text) = choice.message.content.filter(|s| !s.is_empty()) {
        content_blocks.push(ContentBlock::Text { text });
    }

    for tc in choice.message.tool_calls {
        // MEDIUM-11: An upstream that returns malformed JSON in
        // `function.arguments` used to silently null-out the
        // payload, hiding bugs that would later surface deep
        // inside the tool runner. Empty arguments are legal
        // (the tool takes no input); anything else must parse.
        let args_raw = tc.function.arguments.trim();
        let parsed: serde_json::Value = if args_raw.is_empty() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            match serde_json::from_str(args_raw) {
                Ok(v) => v,
                Err(e) => {
                    return Err(LlmError::UpstreamMalformed(format!(
                        "tool_calls[{name}].arguments is not valid JSON: {err}",
                        name = tc.function.name,
                        err = e
                    )));
                }
            }
        };
        content_blocks.push(ContentBlock::ToolUse {
            id: tc.id.clone(),
            name: tc.function.name.clone(),
            input: parsed.clone(),
        });
        tool_calls.push(ToolCall {
            id: tc.id,
            name: tc.function.name,
            input: parsed,
        });
    }

    let finish_reason = match choice.finish_reason.as_deref() {
        Some("stop") | Some("end_turn") | None => FinishReason::Stop,
        Some("length") | Some("max_tokens") => FinishReason::Length,
        Some("tool_calls") | Some("function_call") => FinishReason::ToolUse,
        Some("content_filter") => FinishReason::ContentFilter,
        Some(_) => FinishReason::Other,
    };

    let usage = resp
        .usage
        .map(|u| Usage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            ..Default::default()
        })
        .unwrap_or_default();

    Ok(ChatResponse {
        model: resp.model.unwrap_or_else(|| fallback_model.to_string()),
        content: content_blocks,
        tool_calls,
        finish_reason,
        usage,
    })
}

/// Map a non-2xx HTTP response into the right [`LlmError`].
pub(crate) fn classify_http_error(status: reqwest::StatusCode, body: &[u8]) -> LlmError {
    let body_text = String::from_utf8_lossy(body).to_string();
    let upstream_message = extract_error_message(&body_text);

    match status.as_u16() {
        401 | 403 => LlmError::Auth,
        429 => {
            let retry_after_ms = extract_retry_after_ms(&body_text).unwrap_or(1_000);
            LlmError::RateLimited { retry_after_ms }
        }
        _ => LlmError::Provider {
            status: status.as_u16(),
            message: upstream_message,
        },
    }
}

fn extract_error_message(body: &str) -> String {
    // OpenAI: `{"error":{"message":"...","type":"...","code":"..."}}`
    // DeepSeek / xAI / OpenRouter: similar shape.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(msg) = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
        {
            return crate::agent::llm::redact_body_for_error(msg);
        }
        if let Some(msg) = v.get("message").and_then(|m| m.as_str()) {
            return crate::agent::llm::redact_body_for_error(msg);
        }
    }
    // SECURITY: error bodies routinely echo prompts + key
    // fragments. Run them through the bearer / API-key
    // masking helper before surfacing.
    crate::agent::llm::redact_body_for_error(body)
}

fn extract_retry_after_ms(body: &str) -> Option<u64> {
    // OpenAI returns "Please try again in 1.234s" or similar. Best-effort.
    let s = body.to_lowercase();
    let after = s.split("try again in ").nth(1)?;
    let num: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let secs: f64 = num.parse().ok()?;
    Some((secs * 1000.0) as u64)
}

// -------------------------------------------------------------------
// Streaming
// -------------------------------------------------------------------
//
// OpenAI's `stream=true` shape:
//
//   data: {"choices":[{"delta":{"content":"hi"},"index":0,"finish_reason":null}]}
//   data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"name":"f","arguments":""}}]}}]}
//   data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\""}}]}}]}
//   ...
//   data: [DONE]
//
// Tool-call args arrive incrementally; we buffer them per index
// and emit a single `ToolUse` event with the parsed JSON when
// the stream finishes (or, on `[DONE]`, attempt a final parse).

#[derive(Debug, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<UsageJson>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<StreamToolCall>,
}

#[derive(Debug, Deserialize)]
struct StreamToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamToolFunction>,
}

#[derive(Debug, Deserialize)]
struct StreamToolFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

struct PartialToolCall {
    id: String,
    name: String,
    args_buf: String,
    started: bool,
}

pub(crate) struct OpenAiStreamConverter {
    model: String,
    usage: Usage,
    finish: FinishReason,
    tool_calls: std::collections::BTreeMap<usize, PartialToolCall>,
    finished: bool,
}

impl OpenAiStreamConverter {
    pub(crate) fn new(model: String) -> Self {
        Self {
            model,
            usage: Usage::default(),
            finish: FinishReason::Stop,
            tool_calls: std::collections::BTreeMap::new(),
            finished: false,
        }
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.finished
    }

    /// Process a single SSE event. Returns the StreamEvents to
    /// surface to the caller, in order.
    pub(crate) fn process(
        &mut self,
        sse: &crate::agent::llm::sse::SseEvent,
    ) -> Vec<Result<StreamEvent>> {
        if self.finished {
            return Vec::new();
        }
        let data = sse.data.trim();
        if data.is_empty() {
            return Vec::new();
        }
        if data == "[DONE]" {
            return self.finish_stream();
        }
        let chunk: StreamChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(e) => {
                self.finished = true;
                return vec![Err(LlmError::UpstreamMalformed(format!(
                    "openai stream chunk: {e}"
                )))];
            }
        };

        if let Some(m) = chunk.model {
            self.model = m;
        }
        if let Some(u) = chunk.usage {
            self.usage = Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
                ..Default::default()
            };
        }

        let mut out: Vec<Result<StreamEvent>> = Vec::new();
        for ch in chunk.choices {
            if let Some(text) = ch.delta.content {
                if !text.is_empty() {
                    out.push(Ok(StreamEvent::TextDelta { text }));
                }
            }
            for tc in ch.delta.tool_calls {
                let slot = self
                    .tool_calls
                    .entry(tc.index)
                    .or_insert_with(|| PartialToolCall {
                        id: String::new(),
                        name: String::new(),
                        args_buf: String::new(),
                        started: false,
                    });
                if let Some(id) = tc.id {
                    slot.id = id;
                }
                if let Some(f) = tc.function {
                    if let Some(n) = f.name {
                        if !n.is_empty() {
                            slot.name = n;
                        }
                    }
                    if let Some(args) = f.arguments {
                        // Emit a single ToolUseStart on first
                        // delta for this index, then stream args
                        // as ToolInputDelta. We tolerate the
                        // case where `id` arrives in a later
                        // chunk by using a synthesised id until
                        // it lands.
                        if !slot.started && (!slot.name.is_empty() || !slot.id.is_empty()) {
                            let id = if slot.id.is_empty() {
                                format!("tool_{}", tc.index)
                            } else {
                                slot.id.clone()
                            };
                            let name = slot.name.clone();
                            out.push(Ok(StreamEvent::ToolUseStart { id, name }));
                            slot.started = true;
                        }
                        if !args.is_empty() {
                            slot.args_buf.push_str(&args);
                            if slot.started {
                                out.push(Ok(StreamEvent::ToolInputDelta {
                                    id: if slot.id.is_empty() {
                                        format!("tool_{}", tc.index)
                                    } else {
                                        slot.id.clone()
                                    },
                                    partial_json: args,
                                }));
                            }
                        }
                    }
                }
            }
            if let Some(fr) = ch.finish_reason {
                self.finish = match fr.as_str() {
                    "stop" | "end_turn" => FinishReason::Stop,
                    "length" | "max_tokens" => FinishReason::Length,
                    "tool_calls" | "function_call" => FinishReason::ToolUse,
                    "content_filter" => FinishReason::ContentFilter,
                    _ => FinishReason::Other,
                };
            }
        }
        out
    }

    /// Flush buffered tool calls and emit the terminal `Done`
    /// event. Idempotent — repeated invocations are no-ops once
    /// finished.
    pub(crate) fn finish_stream(&mut self) -> Vec<Result<StreamEvent>> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut out: Vec<Result<StreamEvent>> = Vec::new();
        // Flush any unstarted tool calls (no name yet → impossible
        // but be defensive) and accumulate parsed args into a
        // ToolUse event each.
        let calls = std::mem::take(&mut self.tool_calls);
        for (idx, slot) in calls {
            let id = if slot.id.is_empty() {
                format!("tool_{idx}")
            } else {
                slot.id
            };
            let input: serde_json::Value = if slot.args_buf.trim().is_empty() {
                serde_json::Value::Object(serde_json::Map::new())
            } else {
                match serde_json::from_str(&slot.args_buf) {
                    Ok(v) => v,
                    Err(e) => {
                        out.push(Err(LlmError::UpstreamMalformed(format!(
                            "tool_calls[{name}].arguments: {e}",
                            name = slot.name
                        ))));
                        continue;
                    }
                }
            };
            if !slot.started {
                out.push(Ok(StreamEvent::ToolUseStart {
                    id: id.clone(),
                    name: slot.name.clone(),
                }));
            }
            out.push(Ok(StreamEvent::ToolUse(ToolCall {
                id,
                name: slot.name,
                input,
            })));
        }
        out.push(Ok(StreamEvent::Done {
            finish: self.finish,
            usage: self.usage.clone(),
        }));
        out
    }
}

pub(crate) struct OpenAiStream {
    bytes: BoxStream<'static, std::result::Result<bytes::Bytes, reqwest::Error>>,
    parser: crate::agent::llm::sse::SseParser,
    converter: OpenAiStreamConverter,
    pending: std::collections::VecDeque<Result<StreamEvent>>,
    bytes_done: bool,
    total_bytes: usize,
    pool: Option<Arc<crate::agent::llm::credential_pool::Pool>>,
    lease: Option<crate::agent::llm::credential_pool::Lease>,
    accounted: bool,
}

impl OpenAiStream {
    pub(crate) fn new<S>(
        bytes: S,
        model: String,
        pool: Option<Arc<crate::agent::llm::credential_pool::Pool>>,
        lease: Option<crate::agent::llm::credential_pool::Lease>,
    ) -> Self
    where
        S: futures_util::Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>>
            + Send
            + 'static,
    {
        Self {
            bytes: bytes.boxed(),
            parser: crate::agent::llm::sse::SseParser::new(),
            converter: OpenAiStreamConverter::new(model),
            pending: std::collections::VecDeque::new(),
            bytes_done: false,
            total_bytes: 0,
            pool,
            lease,
            accounted: false,
        }
    }

    fn drain_parser(&mut self) {
        while let Some(sse) = self.parser.pop_event() {
            for ev in self.converter.process(&sse) {
                self.pending.push_back(ev);
            }
        }
    }

    fn surface_overflow(&mut self, e: crate::agent::llm::sse::SseOverflow) {
        self.pending
            .push_back(Err(LlmError::UpstreamMalformed(format!(
                "openai stream: {e}"
            ))));
        self.bytes_done = true;
        self.report_failure_once(crate::agent::llm::credential_pool::FailureClass::Transient);
    }

    fn report_failure_once(&mut self, cls: crate::agent::llm::credential_pool::FailureClass) {
        if self.accounted {
            return;
        }
        self.accounted = true;
        if let (Some(p), Some(l)) = (&self.pool, &self.lease) {
            p.report_failure(l, cls);
        }
    }

    fn report_success_once(&mut self) {
        if self.accounted {
            return;
        }
        self.accounted = true;
        if let (Some(p), Some(l)) = (&self.pool, &self.lease) {
            p.report_success(l);
        }
    }
}

impl futures_util::Stream for OpenAiStream {
    type Item = Result<StreamEvent>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        loop {
            if let Some(ev) = self.pending.pop_front() {
                // Successful completion: when we surface the
                // final `Done` event, credit the lease (MEDIUM-9
                // success-on-DONE).
                if matches!(ev, Ok(StreamEvent::Done { .. })) {
                    self.report_success_once();
                } else if ev.is_err() && !self.accounted {
                    self.report_failure_once(
                        crate::agent::llm::credential_pool::FailureClass::Transient,
                    );
                }
                return Poll::Ready(Some(ev));
            }
            if self.bytes_done {
                return Poll::Ready(None);
            }
            match std::pin::Pin::new(&mut self.bytes).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    if let Err(e) = self.parser.finish() {
                        self.surface_overflow(e);
                        continue;
                    }
                    self.drain_parser();
                    // Stream ended without an explicit [DONE].
                    // Synthesise a Done event so callers can
                    // close out cleanly.
                    if !self.converter.is_finished() {
                        for ev in self.converter.finish_stream() {
                            self.pending.push_back(ev);
                        }
                    }
                    self.bytes_done = true;
                    continue;
                }
                Poll::Ready(Some(Ok(chunk))) => {
                    self.total_bytes = self.total_bytes.saturating_add(chunk.len());
                    if self.total_bytes > crate::agent::llm::MAX_STREAM_TOTAL_BYTES {
                        self.pending
                            .push_back(Err(LlmError::UpstreamMalformed(format!(
                                "openai stream exceeded {} bytes",
                                crate::agent::llm::MAX_STREAM_TOTAL_BYTES
                            ))));
                        self.bytes_done = true;
                        self.report_failure_once(
                            crate::agent::llm::credential_pool::FailureClass::Transient,
                        );
                        continue;
                    }
                    if let Err(e) = self.parser.feed(&chunk) {
                        self.surface_overflow(e);
                        continue;
                    }
                    self.drain_parser();
                    if self.converter.is_finished() {
                        self.bytes_done = true;
                    }
                    continue;
                }
                Poll::Ready(Some(Err(e))) => {
                    self.pending.push_back(Err(LlmError::Transport(e)));
                    self.bytes_done = true;
                    self.report_failure_once(
                        crate::agent::llm::error_classifier::classify_network_error(),
                    );
                    continue;
                }
            }
        }
    }
}
