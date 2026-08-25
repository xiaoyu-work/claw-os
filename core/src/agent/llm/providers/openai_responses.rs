//! OpenAI Responses API wire adapter for Copilot models that expose `/responses`.
//!
//! This module owns request serialization, non-streaming response conversion,
//! and SSE normalization. Wire fields and terminal-event semantics are
//! invariants; credential leases are credited only after terminal success and
//! failed once on malformed or transport termination.

use futures_util::stream::{BoxStream, StreamExt};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Arc;

use super::openai_compat::wire;
use crate::agent::llm::{
    ChatRequest, ChatResponse, ContentBlock, FinishReason, LlmError, Result, Role, StreamEvent,
    ToolCall, ToolChoice, Usage,
};

pub(crate) fn build_request_body(
    request: &ChatRequest,
    model: &str,
    stream: bool,
) -> serde_json::Value {
    let mut input = Vec::with_capacity(request.messages.len() + 1);
    if let Some(system) = request.system.as_deref() {
        input.push(serde_json::json!({
            "type": "message",
            "role": "system",
            "content": [{"type": "input_text", "text": system}],
        }));
    }
    for message in &request.messages {
        input.extend(message_to_input_items(message));
    }

    let tools: Vec<serde_json::Value> = request
        .tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
                "strict": false,
            })
        })
        .collect();

    let mut body = serde_json::json!({
        "model": model,
        "input": input,
        "stream": stream,
        "store": false,
        "include": ["reasoning.encrypted_content"],
    });
    if let Some(object) = body.as_object_mut() {
        if !tools.is_empty() {
            object.insert("tools".into(), serde_json::Value::Array(tools));
            object.insert(
                "tool_choice".into(),
                match &request.tool_choice {
                    ToolChoice::Auto => serde_json::json!("auto"),
                    ToolChoice::None => serde_json::json!("none"),
                    ToolChoice::Required => serde_json::json!("required"),
                    ToolChoice::Tool { name } => {
                        serde_json::json!({"type": "function", "name": name})
                    }
                },
            );
        }
        if let Some(max_tokens) = request.max_tokens {
            object.insert("max_output_tokens".into(), serde_json::json!(max_tokens));
        }
        // Copilot's GPT-5 / reasoning models reject sampling knobs.
        if !wire::use_max_completion_tokens(model) {
            if let Some(temperature) = request.temperature {
                object.insert("temperature".into(), serde_json::json!(temperature));
            }
            if let Some(top_p) = request.top_p {
                object.insert("top_p".into(), serde_json::json!(top_p));
            }
        }
        if let serde_json::Value::Object(extra) = &request.extra {
            for (key, value) in extra {
                if key.starts_with("_cos_") {
                    continue;
                }
                if matches!(
                    key.as_str(),
                    "model" | "input" | "stream" | "store" | "include"
                ) {
                    continue;
                }
                object.insert(key.clone(), value.clone());
            }
        }
    }
    body
}

fn message_to_input_items(message: &crate::agent::llm::Message) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    let mut pending_content = Vec::new();
    let thought_signatures: std::collections::HashMap<&str, &str> = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolState {
                tool_use_id,
                thought_signature,
            } => Some((tool_use_id.as_str(), thought_signature.as_str())),
            _ => None,
        })
        .collect();
    let role = match message.role {
        Role::System => "system",
        Role::Assistant => "assistant",
        Role::User | Role::Tool => "user",
    };
    let flush_content = |items: &mut Vec<serde_json::Value>,
                         content: &mut Vec<serde_json::Value>| {
        if !content.is_empty() {
            items.push(serde_json::json!({
                "type": "message",
                "role": role,
                "content": std::mem::take(content),
            }));
        }
    };

    for block in &message.content {
        match block {
            ContentBlock::Text { text } => {
                let kind = if message.role == Role::Assistant {
                    "output_text"
                } else {
                    "input_text"
                };
                pending_content.push(serde_json::json!({"type": kind, "text": text}));
            }
            ContentBlock::Image { media_type, data } => {
                if message.role == Role::Assistant {
                    pending_content.push(serde_json::json!({
                        "type": "output_text",
                        "text": format!("[image {media_type} base64 attached]"),
                    }));
                } else {
                    pending_content.push(serde_json::json!({
                        "type": "input_image",
                        "image_url": format!("data:{media_type};base64,{data}"),
                        "detail": "auto",
                    }));
                }
            }
            ContentBlock::ToolUse { id, name, input } => {
                flush_content(&mut items, &mut pending_content);
                let mut function_call = serde_json::json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": serde_json::to_string(input)
                        .unwrap_or_else(|_| "{}".to_string()),
                    "status": "completed",
                });
                if let Some(signature) = thought_signatures.get(id.as_str()) {
                    function_call["thought_signature"] =
                        serde_json::Value::String((*signature).to_string());
                }
                items.push(function_call);
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                flush_content(&mut items, &mut pending_content);
                items.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": content,
                }));
            }
            ContentBlock::ToolState { .. } => {
                // Folded into its matching function_call above.
            }
            ContentBlock::Reasoning {
                id,
                summary,
                encrypted_content,
            } => {
                flush_content(&mut items, &mut pending_content);
                // CAPI rejects reasoning IDs from other protocols.
                if let Some(encrypted_content) = encrypted_content
                    .as_deref()
                    .filter(|_| id.starts_with("rs"))
                {
                    items.push(serde_json::json!({
                        "type": "reasoning",
                        "id": id,
                        "summary": summary
                            .iter()
                            .map(|text| serde_json::json!({
                                "type": "summary_text",
                                "text": text,
                            }))
                            .collect::<Vec<_>>(),
                        "encrypted_content": encrypted_content,
                    }));
                }
            }
        }
    }

    flush_content(&mut items, &mut pending_content);
    if items.is_empty() {
        let kind = if message.role == Role::Assistant {
            "output_text"
        } else {
            "input_text"
        };
        items.push(serde_json::json!({
            "type": "message",
            "role": role,
            "content": [{"type": kind, "text": ""}],
        }));
    }
    items
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Response {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    output: Vec<ResponseOutputItem>,
    #[serde(default)]
    usage: Option<ResponseUsage>,
    #[serde(default)]
    incomplete_details: Option<IncompleteDetails>,
    #[serde(default)]
    error: Option<ResponseError>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    input_tokens_details: Option<InputTokenDetails>,
}

#[derive(Debug, Clone, Deserialize)]
struct InputTokenDetails {
    #[serde(default)]
    cached_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct IncompleteDetails {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseOutputItem {
    Message {
        #[serde(default)]
        content: Vec<ResponseOutputContent>,
    },
    FunctionCall {
        #[serde(default)]
        id: Option<String>,
        call_id: String,
        name: String,
        #[serde(default)]
        arguments: String,
        #[serde(default)]
        thought_signature: Option<String>,
    },
    Reasoning {
        id: String,
        #[serde(default)]
        summary: Vec<ResponseReasoningSummary>,
        #[serde(default)]
        encrypted_content: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseReasoningSummary {
    #[serde(default)]
    text: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseOutputContent {
    OutputText {
        text: String,
    },
    Refusal {
        refusal: String,
    },
    #[serde(other)]
    Unknown,
}

pub(crate) fn response_from_slice(bytes: &[u8], fallback_model: &str) -> Result<ChatResponse> {
    let response: Response =
        serde_json::from_slice(bytes).map_err(|error| LlmError::Parse(error.to_string()))?;
    response_to_chat(response, fallback_model)
}

fn response_to_chat(response: Response, fallback_model: &str) -> Result<ChatResponse> {
    if let Some(error) = response.error.as_ref() {
        return Err(response_error(error));
    }
    if response.status.as_deref() == Some("failed") {
        return Err(LlmError::Provider {
            status: 500,
            message: "Copilot Responses API returned failed status".into(),
        });
    }

    let mut content = Vec::new();
    let mut tool_calls = Vec::new();
    let mut saw_refusal = false;
    for item in response.output {
        match item {
            ResponseOutputItem::Message {
                content: message_content,
            } => {
                for part in message_content {
                    match part {
                        ResponseOutputContent::OutputText { text } if !text.is_empty() => {
                            content.push(ContentBlock::Text { text });
                        }
                        ResponseOutputContent::Refusal { refusal } => {
                            saw_refusal = true;
                            if !refusal.is_empty() {
                                content.push(ContentBlock::Text { text: refusal });
                            }
                        }
                        _ => {}
                    }
                }
            }
            ResponseOutputItem::FunctionCall {
                id,
                call_id,
                name,
                arguments,
                thought_signature,
            } => {
                let input = parse_tool_arguments(&name, &arguments)?;
                let call_id = if call_id.is_empty() {
                    id.unwrap_or_else(|| format!("call_{}", tool_calls.len()))
                } else {
                    call_id
                };
                if let Some(thought_signature) = thought_signature {
                    content.push(ContentBlock::ToolState {
                        tool_use_id: call_id.clone(),
                        thought_signature,
                    });
                }
                content.push(ContentBlock::ToolUse {
                    id: call_id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
                tool_calls.push(ToolCall {
                    id: call_id,
                    name,
                    input,
                });
            }
            ResponseOutputItem::Reasoning {
                id,
                summary,
                encrypted_content,
            } => {
                if id.starts_with("rs") {
                    content.push(ContentBlock::Reasoning {
                        id,
                        summary: summary
                            .into_iter()
                            .map(|part| part.text)
                            .filter(|text| !text.is_empty())
                            .collect(),
                        encrypted_content,
                    });
                }
            }
            ResponseOutputItem::Unknown => {}
        }
    }

    let finish_reason = if let Some(details) = response.incomplete_details {
        match details.reason.as_deref() {
            Some("max_output_tokens") => FinishReason::Length,
            Some("content_filter") => FinishReason::ContentFilter,
            _ => FinishReason::Other,
        }
    } else if !tool_calls.is_empty() {
        FinishReason::ToolUse
    } else if saw_refusal {
        FinishReason::Refusal
    } else {
        FinishReason::Stop
    };

    Ok(ChatResponse {
        model: response.model.unwrap_or_else(|| fallback_model.to_string()),
        content,
        tool_calls,
        finish_reason,
        usage: response.usage.map(usage_from_response).unwrap_or_default(),
    })
}

fn parse_tool_arguments(name: &str, arguments: &str) -> Result<serde_json::Value> {
    if arguments.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(arguments).map_err(|error| {
        LlmError::UpstreamMalformed(format!(
            "Responses function_call `{name}` arguments are not valid JSON: {error}"
        ))
    })
}

fn response_error(error: &ResponseError) -> LlmError {
    let message = match (&error.code, &error.message) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        (Some(code), None) => code.clone(),
        (None, Some(message)) => message.clone(),
        (None, None) => "unknown Responses API error".into(),
    };
    let message = crate::agent::llm::redact_body_for_error(&message);
    match error
        .code
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "rate_limit_exceeded" | "rate_limit_error" | "insufficient_quota" => {
            LlmError::RateLimited {
                retry_after_ms: 1_000,
            }
        }
        "authentication_error" | "invalid_api_key" | "unauthorized" | "forbidden" => LlmError::Auth,
        "invalid_request_error"
        | "invalid_request_body"
        | "bad_request"
        | "model_not_supported" => LlmError::InvalidRequest(message),
        "service_unavailable" => LlmError::Provider {
            status: 503,
            message,
        },
        _ => LlmError::Provider {
            // `response.failed` is a terminal upstream failure even
            // though the HTTP stream itself returned 200.
            status: 500,
            message,
        },
    }
}

fn usage_from_response(usage: ResponseUsage) -> Usage {
    Usage {
        input_tokens: saturating_u32(usage.input_tokens),
        output_tokens: saturating_u32(usage.output_tokens),
        cache_read_tokens: saturating_u32(
            usage
                .input_tokens_details
                .map(|details| details.cached_tokens)
                .unwrap_or_default(),
        ),
        ..Default::default()
    }
}

fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ResponsesEvent {
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        error: Option<ResponseError>,
    },
    #[serde(rename = "response.created")]
    Created { response: Response },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded {
        output_index: usize,
        item: ResponseOutputItem,
    },
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta { delta: String },
    #[serde(rename = "response.refusal.delta")]
    RefusalDelta { delta: String },
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionArgumentsDelta { output_index: usize, delta: String },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone {
        output_index: usize,
        item: ResponseOutputItem,
    },
    #[serde(rename = "response.completed")]
    Completed { response: Response },
    #[serde(rename = "response.incomplete")]
    Incomplete { response: Response },
    #[serde(rename = "response.failed")]
    Failed { response: Response },
    #[serde(other)]
    Unknown,
}

struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
    thought_signature: Option<String>,
    started: bool,
}

pub(crate) struct ResponsesStreamConverter {
    model: String,
    usage: Usage,
    finish: FinishReason,
    tool_calls: BTreeMap<usize, PartialToolCall>,
    emitted_call_ids: HashSet<String>,
    emitted_reasoning_ids: HashSet<String>,
    saw_tool_call: bool,
    saw_refusal: bool,
    finished: bool,
}

impl ResponsesStreamConverter {
    pub(crate) fn new(model: String) -> Self {
        Self {
            model,
            usage: Usage::default(),
            finish: FinishReason::Stop,
            tool_calls: BTreeMap::new(),
            emitted_call_ids: HashSet::new(),
            emitted_reasoning_ids: HashSet::new(),
            saw_tool_call: false,
            saw_refusal: false,
            finished: false,
        }
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.finished
    }

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
        let event: ResponsesEvent = match serde_json::from_str(data) {
            Ok(event) => event,
            Err(error) => {
                self.finished = true;
                return vec![Err(LlmError::UpstreamMalformed(format!(
                    "responses stream event: {error}"
                )))];
            }
        };

        match event {
            ResponsesEvent::Created { response } => {
                self.update_response_metadata(&response);
                Vec::new()
            }
            ResponsesEvent::OutputTextDelta { delta } => {
                if delta.is_empty() {
                    Vec::new()
                } else {
                    vec![Ok(StreamEvent::TextDelta { text: delta })]
                }
            }
            ResponsesEvent::RefusalDelta { delta } => {
                self.saw_refusal = true;
                if delta.is_empty() {
                    Vec::new()
                } else {
                    vec![Ok(StreamEvent::TextDelta { text: delta })]
                }
            }
            ResponsesEvent::OutputItemAdded { output_index, item } => {
                self.start_output_item(output_index, item)
            }
            ResponsesEvent::FunctionArgumentsDelta {
                output_index,
                delta,
            } => self.tool_arguments_delta(output_index, delta),
            ResponsesEvent::OutputItemDone { output_index, item } => {
                self.complete_output_item(output_index, item)
            }
            ResponsesEvent::Completed { response } => self.finish_from_response(response, None),
            ResponsesEvent::Incomplete { response } => {
                let finish = match response
                    .incomplete_details
                    .as_ref()
                    .and_then(|details| details.reason.as_deref())
                {
                    Some("max_output_tokens") => FinishReason::Length,
                    Some("content_filter") => FinishReason::ContentFilter,
                    _ => FinishReason::Other,
                };
                self.finish_from_response(response, Some(finish))
            }
            ResponsesEvent::Failed { response } => {
                self.finished = true;
                vec![Err(response
                    .error
                    .as_ref()
                    .map(response_error)
                    .unwrap_or_else(|| LlmError::Provider {
                        status: 500,
                        message: "Copilot Responses API returned response.failed".into(),
                    }))]
            }
            ResponsesEvent::Error {
                code,
                message,
                error,
            } => {
                self.finished = true;
                let error = error.unwrap_or(ResponseError { code, message });
                vec![Err(response_error(&error))]
            }
            ResponsesEvent::Unknown => Vec::new(),
        }
    }

    fn start_output_item(
        &mut self,
        output_index: usize,
        item: ResponseOutputItem,
    ) -> Vec<Result<StreamEvent>> {
        let ResponseOutputItem::FunctionCall {
            id,
            call_id,
            name,
            arguments,
            thought_signature,
        } = item
        else {
            return Vec::new();
        };
        self.saw_tool_call = true;
        let id = if call_id.is_empty() {
            id.unwrap_or_else(|| format!("call_{output_index}"))
        } else {
            call_id
        };
        self.tool_calls.insert(
            output_index,
            PartialToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments,
                thought_signature,
                started: true,
            },
        );
        vec![Ok(StreamEvent::ToolUseStart { id, name })]
    }

    fn tool_arguments_delta(
        &mut self,
        output_index: usize,
        delta: String,
    ) -> Vec<Result<StreamEvent>> {
        if delta.is_empty() {
            return Vec::new();
        }
        let tool = self
            .tool_calls
            .entry(output_index)
            .or_insert_with(|| PartialToolCall {
                id: format!("call_{output_index}"),
                name: String::new(),
                arguments: String::new(),
                thought_signature: None,
                started: false,
            });
        tool.arguments.push_str(&delta);
        let mut out = Vec::new();
        if !tool.started {
            out.push(Ok(StreamEvent::ToolUseStart {
                id: tool.id.clone(),
                name: tool.name.clone(),
            }));
            tool.started = true;
        }
        out.push(Ok(StreamEvent::ToolInputDelta {
            id: tool.id.clone(),
            partial_json: delta,
        }));
        out
    }

    fn complete_output_item(
        &mut self,
        output_index: usize,
        item: ResponseOutputItem,
    ) -> Vec<Result<StreamEvent>> {
        let (id, call_id, name, arguments, thought_signature) = match item {
            ResponseOutputItem::FunctionCall {
                id,
                call_id,
                name,
                arguments,
                thought_signature,
            } => (id, call_id, name, arguments, thought_signature),
            ResponseOutputItem::Reasoning {
                id,
                summary,
                encrypted_content,
            } => {
                if id.starts_with("rs") && self.emitted_reasoning_ids.insert(id.clone()) {
                    return vec![Ok(StreamEvent::Reasoning {
                        id,
                        summary: summary
                            .into_iter()
                            .map(|part| part.text)
                            .filter(|text| !text.is_empty())
                            .collect(),
                        encrypted_content,
                    })];
                }
                return Vec::new();
            }
            _ => return Vec::new(),
        };
        self.saw_tool_call = true;
        let partial = self.tool_calls.remove(&output_index);
        let id = if call_id.is_empty() {
            id.or_else(|| partial.as_ref().map(|value| value.id.clone()))
                .unwrap_or_else(|| format!("call_{output_index}"))
        } else {
            call_id
        };
        if !self.emitted_call_ids.insert(id.clone()) {
            return Vec::new();
        }
        let name = if name.is_empty() {
            partial
                .as_ref()
                .map(|value| value.name.clone())
                .unwrap_or_default()
        } else {
            name
        };
        let arguments = if arguments.is_empty() {
            partial
                .as_ref()
                .map(|value| value.arguments.clone())
                .unwrap_or_default()
        } else {
            arguments
        };
        let input = match parse_tool_arguments(&name, &arguments) {
            Ok(input) => input,
            Err(error) => return vec![Err(error)],
        };
        let mut out = Vec::new();
        if let Some(thought_signature) = thought_signature.or_else(|| {
            partial
                .as_ref()
                .and_then(|value| value.thought_signature.clone())
        }) {
            out.push(Ok(StreamEvent::ToolState {
                tool_use_id: id.clone(),
                thought_signature,
            }));
        }
        if !partial.as_ref().is_some_and(|value| value.started) {
            out.push(Ok(StreamEvent::ToolUseStart {
                id: id.clone(),
                name: name.clone(),
            }));
        }
        out.push(Ok(StreamEvent::ToolUse(ToolCall { id, name, input })));
        out
    }

    fn finish_from_response(
        &mut self,
        response: Response,
        forced_finish: Option<FinishReason>,
    ) -> Vec<Result<StreamEvent>> {
        if let Some(error) = response.error.as_ref() {
            self.finished = true;
            return vec![Err(response_error(error))];
        }
        self.update_response_metadata(&response);
        let mut out = Vec::new();
        for (index, item) in response.output.into_iter().enumerate() {
            if matches!(
                item,
                ResponseOutputItem::FunctionCall { .. } | ResponseOutputItem::Reasoning { .. }
            ) {
                out.extend(self.complete_output_item(index, item));
            }
        }
        self.finish = forced_finish.unwrap_or({
            if self.saw_tool_call {
                FinishReason::ToolUse
            } else if self.saw_refusal {
                FinishReason::Refusal
            } else {
                FinishReason::Stop
            }
        });
        out.extend(self.finish_stream());
        out
    }

    fn update_response_metadata(&mut self, response: &Response) {
        if let Some(model) = response.model.as_ref() {
            self.model = model.clone();
        }
        if let Some(usage) = response.usage.clone() {
            self.usage = usage_from_response(usage);
        }
    }

    pub(crate) fn finish_stream(&mut self) -> Vec<Result<StreamEvent>> {
        if self.finished {
            return Vec::new();
        }
        if self.finish == FinishReason::Stop {
            self.finish = if self.saw_tool_call {
                FinishReason::ToolUse
            } else if self.saw_refusal {
                FinishReason::Refusal
            } else {
                FinishReason::Stop
            };
        }
        self.finished = true;
        let mut out = Vec::new();
        let pending = std::mem::take(&mut self.tool_calls);
        for (index, tool) in pending {
            if self.emitted_call_ids.contains(&tool.id) {
                continue;
            }
            let input = match parse_tool_arguments(&tool.name, &tool.arguments) {
                Ok(input) => input,
                Err(error) => {
                    out.push(Err(error));
                    continue;
                }
            };
            let id = if tool.id.is_empty() {
                format!("call_{index}")
            } else {
                tool.id
            };
            if let Some(thought_signature) = tool.thought_signature {
                out.push(Ok(StreamEvent::ToolState {
                    tool_use_id: id.clone(),
                    thought_signature,
                }));
            }
            if !tool.started {
                out.push(Ok(StreamEvent::ToolUseStart {
                    id: id.clone(),
                    name: tool.name.clone(),
                }));
            }
            out.push(Ok(StreamEvent::ToolUse(ToolCall {
                id,
                name: tool.name,
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

pub(crate) struct ResponsesStream {
    bytes: BoxStream<'static, std::result::Result<bytes::Bytes, reqwest::Error>>,
    parser: crate::agent::llm::sse::SseParser,
    converter: ResponsesStreamConverter,
    pending: VecDeque<Result<StreamEvent>>,
    bytes_done: bool,
    total_bytes: usize,
    pool: Option<Arc<crate::agent::llm::credential_pool::Pool>>,
    lease: Option<crate::agent::llm::credential_pool::Lease>,
    accounted: bool,
}

impl ResponsesStream {
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
            converter: ResponsesStreamConverter::new(model),
            pending: VecDeque::new(),
            bytes_done: false,
            total_bytes: 0,
            pool,
            lease,
            accounted: false,
        }
    }

    fn drain_parser(&mut self) {
        while let Some(sse) = self.parser.pop_event() {
            self.pending.extend(self.converter.process(&sse));
        }
    }

    fn report_failure_once(&mut self, class: crate::agent::llm::credential_pool::FailureClass) {
        if self.accounted {
            return;
        }
        self.accounted = true;
        if let (Some(pool), Some(lease)) = (&self.pool, &self.lease) {
            pool.report_failure(lease, class);
        }
    }

    fn report_success_once(&mut self) {
        if self.accounted {
            return;
        }
        self.accounted = true;
        if let (Some(pool), Some(lease)) = (&self.pool, &self.lease) {
            pool.report_success(lease);
        }
    }

    fn fail_stream(&mut self, error: LlmError) {
        self.pending.push_back(Err(error));
        self.bytes_done = true;
        self.report_failure_once(crate::agent::llm::credential_pool::FailureClass::Transient);
    }
}

impl futures_util::Stream for ResponsesStream {
    type Item = Result<StreamEvent>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        loop {
            if let Some(event) = self.pending.pop_front() {
                if matches!(event, Ok(StreamEvent::Done { .. })) {
                    self.report_success_once();
                } else if event.is_err() {
                    self.report_failure_once(
                        crate::agent::llm::credential_pool::FailureClass::Transient,
                    );
                }
                return Poll::Ready(Some(event));
            }
            if self.bytes_done {
                return Poll::Ready(None);
            }
            match std::pin::Pin::new(&mut self.bytes).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    if let Err(error) = self.parser.finish() {
                        self.fail_stream(LlmError::UpstreamMalformed(format!(
                            "responses stream: {error}"
                        )));
                        continue;
                    }
                    self.drain_parser();
                    if !self.converter.is_finished() {
                        self.fail_stream(LlmError::UpstreamMalformed(
                            "responses stream ended before a terminal event".into(),
                        ));
                        continue;
                    }
                    self.bytes_done = true;
                }
                Poll::Ready(Some(Ok(chunk))) => {
                    self.total_bytes = self.total_bytes.saturating_add(chunk.len());
                    if self.total_bytes > crate::agent::llm::MAX_STREAM_TOTAL_BYTES {
                        self.fail_stream(LlmError::UpstreamMalformed(format!(
                            "responses stream exceeded {} bytes",
                            crate::agent::llm::MAX_STREAM_TOTAL_BYTES
                        )));
                        continue;
                    }
                    if let Err(error) = self.parser.feed(&chunk) {
                        self.fail_stream(LlmError::UpstreamMalformed(format!(
                            "responses stream: {error}"
                        )));
                        continue;
                    }
                    self.drain_parser();
                    if self.converter.is_finished() {
                        self.bytes_done = true;
                    }
                }
                Poll::Ready(Some(Err(error))) => {
                    self.pending.push_back(Err(LlmError::Transport(error)));
                    self.bytes_done = true;
                    self.report_failure_once(
                        crate::agent::llm::error_classifier::classify_network_error(),
                    );
                }
            }
        }
    }
}
