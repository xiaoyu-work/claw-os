use serde_json::{json, Value};

use crate::agent::llm::{ContentBlock, StreamEvent};

use super::protocol::{backend_string, notification, safe_text, RpcError, MAX_MESSAGE_BYTES};

const MAX_TURN_ITEMS: usize = 512;

pub(super) struct Projection {
    pub thread_id: String,
    pub session_id: String,
    pub task_id: String,
    items: Vec<Value>,
    text_item: Option<usize>,
    text: TextProjection,
    provider_had_text: bool,
    had_text: bool,
    provider_index: usize,
    open_reasoning: Vec<usize>,
    total_input: u64,
    total_output: u64,
    total_cached: u64,
    total_cache_write: u64,
    visible_bytes: usize,
}

#[derive(Default)]
pub(super) struct Projected {
    pub messages: Vec<Value>,
    pub approvals: Vec<String>,
    pub resumed: bool,
}

impl Projection {
    pub fn new(thread_id: String, session_id: String, task_id: String) -> Self {
        Self {
            thread_id,
            session_id,
            task_id,
            items: Vec::new(),
            text_item: None,
            text: TextProjection::default(),
            provider_had_text: false,
            had_text: false,
            provider_index: 0,
            open_reasoning: Vec::new(),
            total_input: 0,
            total_output: 0,
            total_cached: 0,
            total_cache_write: 0,
            visible_bytes: 0,
        }
    }

    pub fn begin(&mut self, prompt: &str, client_id: Option<&str>, job: &Value) -> Vec<Value> {
        let user = json!({
            "type": "userMessage",
            "id": format!("{}:user", self.task_id),
            "clientId": client_id,
            "content": [{ "type": "text", "text": safe_text(prompt), "text_elements": [] }],
        });
        self.items.push(user.clone());
        vec![
            notification(
                "turn/started",
                json!({
                    "threadId": self.thread_id,
                    "turn": turn_view(&self.task_id, Vec::new(), "inProgress", None, job),
                }),
            ),
            self.status(false),
            self.item_event("item/started", &user, timestamp_ms(job.get("created_at"))),
            self.item_event("item/completed", &user, timestamp_ms(job.get("created_at"))),
        ]
    }

    pub fn record(&mut self, record: &Value) -> Result<Projected, RpcError> {
        let mut output = Projected::default();
        let timestamp = timestamp_ms(record.get("ts"));
        if let Some(progress) = record.get("progress") {
            let kind = backend_string(progress, "kind")?;
            match kind {
                "tool_start" => self.tool_start(
                    backend_string(progress, "id")?,
                    backend_string(progress, "name")?,
                    timestamp,
                    &mut output.messages,
                )?,
                "tool_result" => {
                    let id = backend_string(progress, "id")?;
                    let name = backend_string(progress, "name")?;
                    let ok = progress
                        .get("ok")
                        .and_then(Value::as_bool)
                        .ok_or_else(|| RpcError::backend("Tool result has no outcome"))?;
                    self.tool_start(id, name, timestamp, &mut output.messages)?;
                    let index = self.tool_index(id).ok_or_else(|| {
                        RpcError::backend("Tool result has no corresponding item")
                    })?;
                    if self.items[index]["status"] != "inProgress" {
                        return Ok(output);
                    }
                    self.items[index]["status"] = json!(if ok { "completed" } else { "failed" });
                    self.items[index]["success"] = json!(ok);
                    self.items[index]["durationMs"] = progress
                        .get("latency_ms")
                        .and_then(Value::as_u64)
                        .and_then(|value| i64::try_from(value).ok())
                        .map_or(Value::Null, |value| json!(value));
                    output.messages.push(self.item_event(
                        "item/completed",
                        &self.items[index],
                        timestamp,
                    ));
                }
                "waiting_approval" => {
                    let ids = progress
                        .get("request_ids")
                        .and_then(Value::as_array)
                        .ok_or_else(|| RpcError::backend("Approval wait has no request ids"))?;
                    if ids.is_empty() || ids.len() > 64 {
                        return Err(RpcError::backend("Invalid approval request count"));
                    }
                    for id in ids {
                        let id = id
                            .as_str()
                            .ok_or_else(|| RpcError::backend("Invalid approval request id"))?;
                        super::protocol::token(id, "approval id")?;
                        output.approvals.push(id.to_string());
                    }
                    output.messages.push(self.status(true));
                }
                "approval_resumed" => {
                    output.resumed = true;
                    output.messages.push(self.status(false));
                }
                _ => output.messages.push(self.warning(
                    "Claw reported a task progress type that this TUI adapter cannot display",
                )),
            }
            return Ok(output);
        }
        let event = record
            .get("event")
            .ok_or_else(|| RpcError::backend("Task stream record has no event or progress"))?;
        let event: StreamEvent = serde_json::from_value(event.clone())
            .map_err(|_| RpcError::backend("Claw returned an unsupported task stream event"))?;
        match event {
            StreamEvent::TextDelta { text } => {
                self.provider_had_text = true;
                self.text_delta(&text, timestamp, &mut output.messages)?;
            }
            StreamEvent::Message(response) => {
                if !self.provider_had_text {
                    let text = response
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<String>();
                    self.text_delta(&text, timestamp, &mut output.messages)?;
                }
                for block in &response.content {
                    match block {
                        ContentBlock::Reasoning { id, summary, .. } => {
                            self.reasoning(id, summary, timestamp, &mut output.messages)?;
                        }
                        ContentBlock::ToolUse { id, name, .. } => {
                            self.tool_start(id, name, timestamp, &mut output.messages)?;
                        }
                        _ => {}
                    }
                }
                for call in response.tool_calls {
                    self.tool_start(&call.id, &call.name, timestamp, &mut output.messages)?;
                }
            }
            StreamEvent::ToolUseStart { id, name } => {
                self.tool_start(&id, &name, timestamp, &mut output.messages)?;
            }
            StreamEvent::ToolUse(call) => {
                self.tool_start(&call.id, &call.name, timestamp, &mut output.messages)?;
            }
            StreamEvent::Reasoning { id, summary, .. } => {
                self.reasoning(&id, &summary, timestamp, &mut output.messages)?;
            }
            // Inputs, signatures and opaque reasoning are not a presentation API.
            StreamEvent::ToolInputDelta { .. } | StreamEvent::ToolState { .. } => {}
            StreamEvent::Done { usage, .. } => {
                self.finish_provider(timestamp, &mut output.messages)?;
                self.total_input += u64::from(usage.input_tokens);
                self.total_output += u64::from(usage.output_tokens);
                self.total_cached += u64::from(usage.cache_read_tokens);
                self.total_cache_write += u64::from(usage.cache_write_tokens);
                output.messages.push(notification("thread/tokenUsage/updated", json!({
                    "threadId": self.thread_id,
                    "turnId": self.task_id,
                    "tokenUsage": {
                        "total": usage_view(self.total_input, self.total_output, self.total_cached, self.total_cache_write),
                        "last": usage_view(
                            u64::from(usage.input_tokens),
                            u64::from(usage.output_tokens),
                            u64::from(usage.cache_read_tokens),
                            u64::from(usage.cache_write_tokens),
                        ),
                        "modelContextWindow": null,
                    },
                })));
            }
            StreamEvent::Warning { message } => output.messages.push(self.warning(&message)),
        }
        Ok(output)
    }

    pub fn finish(&mut self, job: &Value) -> Result<Vec<Value>, RpcError> {
        if backend_string(job, "id")? != self.task_id
            || backend_string(job, "session_id")? != self.session_id
        {
            return Err(RpcError::backend("Terminal task identity changed"));
        }
        let status = match backend_string(job, "status")? {
            "ok" => "completed",
            "cancelled" => "interrupted",
            "error" => "failed",
            _ => {
                return Err(RpcError::backend(
                    "Task stream ended before the task was terminal",
                ))
            }
        };
        let mut messages = Vec::new();
        let timestamp = timestamp_ms(job.get("finished_at"));
        if !self.had_text {
            if let Some(answer) = job.get("response").and_then(Value::as_str) {
                self.text_delta(answer, timestamp, &mut messages)?;
            }
        }
        self.finish_provider(timestamp, &mut messages)?;
        for item in &mut self.items {
            if item["type"] == "dynamicToolCall" && item["status"] == "inProgress" {
                item["status"] = json!("failed");
                item["success"] = Value::Null;
                item["contentItems"] = json!([{
                    "type": "inputText",
                    "text": "The task ended without a reported outcome for this tool. No success is implied.",
                }]);
                messages.push(notification(
                    "item/completed",
                    json!({
                        "threadId": self.thread_id,
                        "turnId": self.task_id,
                        "item": item,
                        "completedAtMs": timestamp,
                    }),
                ));
            }
        }
        let error = if status == "failed" {
            Some(turn_error(
                job.get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Claw task failed"),
            ))
        } else {
            None
        };
        if let Some(error) = &error {
            messages.push(notification(
                "error",
                json!({
                    "threadId": self.thread_id,
                    "turnId": self.task_id,
                    "error": error,
                    "willRetry": false,
                }),
            ));
        }
        messages.push(notification(
            "turn/completed",
            json!({
                "threadId": self.thread_id,
                "turn": turn_view(&self.task_id, self.items.clone(), status, error, job),
            }),
        ));
        messages.push(notification(
            "thread/status/changed",
            json!({
                "threadId": self.thread_id,
                "status": { "type": "idle" },
            }),
        ));
        Ok(messages)
    }

    pub fn stream_error(&self, error: &RpcError) -> Value {
        notification(
            "error",
            json!({
                "threadId": self.thread_id,
                "turnId": self.task_id,
                "error": turn_error(&format!(
                    "{}. The durable task may still be running; reconnect to inspect or interrupt it.",
                    error.message
                )),
                "willRetry": false,
            }),
        )
    }

    pub fn snapshot(&self, job: &Value) -> Value {
        turn_view(&self.task_id, self.items.clone(), "inProgress", None, job)
    }

    fn text_delta(
        &mut self,
        text: &str,
        timestamp: i64,
        messages: &mut Vec<Value>,
    ) -> Result<(), RpcError> {
        if text.is_empty() {
            return Ok(());
        }
        self.reserve_text(text.len())?;
        self.had_text = true;
        let index = match self.text_item {
            Some(index) => index,
            None => {
                self.ensure_capacity()?;
                let item = json!({
                    "type": "agentMessage",
                    "id": format!("{}:message:{}", self.task_id, self.provider_index),
                    "text": "",
                    "phase": null,
                    "memoryCitation": null,
                });
                messages.push(self.item_event("item/started", &item, timestamp));
                self.items.push(item);
                let index = self.items.len() - 1;
                self.text_item = Some(index);
                index
            }
        };
        let delta = self.text.push(text, false)?;
        self.emit_text(index, delta, messages);
        Ok(())
    }

    fn emit_text(&mut self, index: usize, delta: String, messages: &mut Vec<Value>) {
        if delta.is_empty() {
            return;
        }
        self.items[index]["text"] = json!(self.text.emitted);
        messages.push(notification(
            "item/agentMessage/delta",
            json!({
                "threadId": self.thread_id,
                "turnId": self.task_id,
                "itemId": self.items[index]["id"],
                "delta": delta,
            }),
        ));
    }

    fn finish_provider(
        &mut self,
        timestamp: i64,
        messages: &mut Vec<Value>,
    ) -> Result<(), RpcError> {
        if let Some(index) = self.text_item.take() {
            let delta = self.text.push("", true)?;
            self.emit_text(index, delta, messages);
            messages.push(self.item_event("item/completed", &self.items[index], timestamp));
        }
        for index in std::mem::take(&mut self.open_reasoning) {
            messages.push(self.item_event("item/completed", &self.items[index], timestamp));
        }
        self.text = TextProjection::default();
        self.provider_had_text = false;
        self.provider_index += 1;
        Ok(())
    }

    fn tool_start(
        &mut self,
        id: &str,
        name: &str,
        timestamp: i64,
        messages: &mut Vec<Value>,
    ) -> Result<(), RpcError> {
        super::protocol::token(id, "tool id")?;
        if name.is_empty() || name.len() > 512 {
            return Err(RpcError::backend("Invalid tool identity"));
        }
        if let Some(index) = self.tool_index(id) {
            self.items[index]["tool"] = json!(safe_text(name));
            return Ok(());
        }
        self.ensure_capacity()?;
        let item = tool_item(id, name, "inProgress", None);
        messages.push(self.item_event("item/started", &item, timestamp));
        self.items.push(item);
        Ok(())
    }

    fn tool_index(&self, id: &str) -> Option<usize> {
        self.items
            .iter()
            .position(|item| item["type"] == "dynamicToolCall" && item["id"].as_str() == Some(id))
    }

    fn reasoning(
        &mut self,
        id: &str,
        summary: &[String],
        timestamp: i64,
        messages: &mut Vec<Value>,
    ) -> Result<(), RpcError> {
        if summary.is_empty() {
            return Ok(());
        }
        let id = if id.is_empty() {
            format!("{}:summary:{}", self.task_id, self.items.len())
        } else {
            id.to_string()
        };
        super::protocol::token(&id, "reasoning summary id")?;
        if self.items.iter().any(|item| item["id"] == id) {
            return Ok(());
        }
        self.reserve_text(summary.iter().map(String::len).sum::<usize>())?;
        self.ensure_capacity()?;
        let mut item = json!({ "type": "reasoning", "id": id, "summary": [], "content": [] });
        messages.push(self.item_event("item/started", &item, timestamp));
        let summary: Vec<String> = summary.iter().map(|text| safe_text(text)).collect();
        for (index, text) in summary.iter().enumerate() {
            messages.push(notification(
                "item/reasoning/summaryPartAdded",
                json!({
                    "threadId": self.thread_id,
                    "turnId": self.task_id,
                    "itemId": id,
                    "summaryIndex": index,
                }),
            ));
            messages.push(notification(
                "item/reasoning/summaryTextDelta",
                json!({
                    "threadId": self.thread_id,
                    "turnId": self.task_id,
                    "itemId": id,
                    "summaryIndex": index,
                    "delta": text,
                }),
            ));
        }
        item["summary"] = json!(summary);
        self.open_reasoning.push(self.items.len());
        self.items.push(item);
        Ok(())
    }

    fn ensure_capacity(&self) -> Result<(), RpcError> {
        if self.items.len() >= MAX_TURN_ITEMS {
            return Err(RpcError::capacity());
        }
        Ok(())
    }

    fn reserve_text(&mut self, bytes: usize) -> Result<(), RpcError> {
        if self.visible_bytes.saturating_add(bytes) > MAX_MESSAGE_BYTES / 2 {
            return Err(RpcError::capacity());
        }
        self.visible_bytes += bytes;
        Ok(())
    }

    fn item_event(&self, method: &str, item: &Value, timestamp: i64) -> Value {
        let mut params = json!({
            "threadId": self.thread_id,
            "turnId": self.task_id,
            "item": item,
        });
        params[if method == "item/started" {
            "startedAtMs"
        } else {
            "completedAtMs"
        }] = json!(timestamp);
        notification(method, params)
    }

    fn status(&self, waiting: bool) -> Value {
        notification(
            "thread/status/changed",
            json!({
                "threadId": self.thread_id,
                "status": {
                    "type": "active",
                    "activeFlags": if waiting { vec!["waitingOnApproval"] } else { Vec::<&str>::new() },
                },
            }),
        )
    }

    fn warning(&self, message: &str) -> Value {
        notification(
            "warning",
            json!({ "threadId": self.thread_id, "message": safe_text(message) }),
        )
    }
}

pub(super) fn tool_item(id: &str, name: &str, status: &str, success: Option<bool>) -> Value {
    json!({
        "type": "dynamicToolCall",
        "id": id,
        "namespace": "claw",
        "tool": safe_text(name),
        "arguments": null,
        "status": status,
        "contentItems": null,
        "success": success,
        "durationMs": null,
    })
}

pub(super) fn turn_error(message: &str) -> Value {
    json!({ "message": safe_text(message), "codexErrorInfo": null, "additionalDetails": null })
}

pub(super) fn turn_view(
    id: &str,
    items: Vec<Value>,
    status: &str,
    error: Option<Value>,
    job: &Value,
) -> Value {
    let started = iso_seconds(job.get("started_at")).or_else(|| iso_seconds(job.get("created_at")));
    let completed = iso_seconds(job.get("finished_at"));
    json!({
        "id": id,
        "items": items,
        "itemsView": "full",
        "status": status,
        "error": error,
        "startedAt": started,
        "completedAt": completed,
        "durationMs": null,
    })
}

pub(super) fn iso_seconds(value: Option<&Value>) -> Option<i64> {
    value
        .and_then(Value::as_str)
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|time| time.timestamp())
}

fn timestamp_ms(value: Option<&Value>) -> i64 {
    value
        .and_then(Value::as_str)
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|time| time.timestamp_millis())
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis())
}

fn usage_view(input: u64, output: u64, cached: u64, cache_write: u64) -> Value {
    json!({
        "totalTokens": input + output,
        "inputTokens": input,
        "cachedInputTokens": cached,
        "cacheWriteInputTokens": cache_write,
        "outputTokens": output,
        "reasoningOutputTokens": 0,
    })
}

/// Hold unfinished tokens and PEM blocks so a credential split across provider
/// deltas cannot evade the ordinary Claw presentation redactor.
#[derive(Default)]
struct TextProjection {
    raw: String,
    emitted: String,
}

impl TextProjection {
    fn push(&mut self, text: &str, complete: bool) -> Result<String, RpcError> {
        if self.raw.len().saturating_add(text.len()) > MAX_MESSAGE_BYTES / 2 {
            return Err(RpcError::capacity());
        }
        self.raw.push_str(text);
        let mut boundary = if complete {
            self.raw.len()
        } else {
            self.raw
                .char_indices()
                .rev()
                .find(|(index, character)| {
                    *index <= self.raw.len().saturating_sub(128) && character.is_whitespace()
                })
                .map_or(0, |(index, _)| index)
        };
        let mut search = 0;
        while let Some(relative_start) = self.raw[search..].find("-----BEGIN ") {
            let start = search + relative_start;
            let label_start = start + "-----BEGIN ".len();
            let end = self.raw[label_start..]
                .find("-----")
                .filter(|length| *length <= 128)
                .and_then(|length| {
                    let marker = format!(
                        "-----END {}-----",
                        &self.raw[label_start..label_start + length]
                    );
                    self.raw[label_start..]
                        .find(&marker)
                        .map(|offset| label_start + offset + marker.len())
                });
            let Some(end) = end else {
                boundary = boundary.min(start);
                if complete {
                    return self.emit(format!(
                        "{}[REDACTED:private_key]",
                        safe_text(&self.raw[..boundary])
                    ));
                }
                break;
            };
            if boundary > start && boundary < end {
                boundary = start;
            }
            search = end;
        }
        self.emit(safe_text(&self.raw[..boundary]))
    }

    fn emit(&mut self, visible: String) -> Result<String, RpcError> {
        let delta = visible
            .strip_prefix(&self.emitted)
            .ok_or_else(|| RpcError::backend("Text redaction changed an already emitted prefix"))?
            .to_string();
        self.emitted = visible;
        Ok(delta)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tui_backend/events.rs"
    ));
}
