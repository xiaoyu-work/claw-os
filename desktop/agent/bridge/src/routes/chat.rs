//! `POST /api/chat` — Server-Sent Events stream of one chat turn.
//!
//! Submits the prompt to `clawd` and re-frames the daemon response as SSE:
//!
//! * submitted task/session identity → `task`
//! * incremental answer text → `delta`
//! * tool lifecycle → `tool_use_start`, `tool_use`, `tool_start`, `tool_result`
//! * recoverable provider notices → `warning` / `turn_done`
//! * final task envelope → `done`
//! * daemon / IO failures → `error`

use std::convert::Infallible;
use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::state::AppState;
use clawd_client::{Client, Command};

/// Hard ceiling on a single chat turn. `task.stream` blocks ~1s per poll, so
/// without this a task that never reports `terminal` (stuck agent, or a frame
/// that never advances the cursor) would keep the SSE connection open and
/// re-poll clawd once a second indefinitely while the client stays connected.
const MAX_STREAM_DURATION: Duration = Duration::from_secs(30 * 60);

struct CancelOnDrop {
    clawd: Client,
    task_id: String,
    armed: bool,
}

impl CancelOnDrop {
    fn new(clawd: Client, task_id: String) -> Self {
        Self {
            clawd,
            task_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let clawd = self.clawd.clone();
        let task_id = self.task_id.clone();
        handle.spawn(async move {
            if let Err(error) = clawd
                .call(Command::TaskCancel, json!({ "id": task_id }))
                .await
            {
                tracing::warn!(%error, "failed to cancel disconnected agent task");
            }
        });
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ChatRequest {
    /// Single-prompt form used by the simple chat shell.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Multi-turn form for richer clients. We pick the last `user`
    /// message and feed it to `clawd` for now; full history
    /// will land once the agent kernel grows a structured-NDJSON
    /// variant.
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    /// Reserved: future agent session_id pinning.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Reserved: future model override (`cos agent ask` doesn't take
    /// `--model` today).
    #[serde(default)]
    pub model: Option<String>,
    /// Transient app/window context shown to the model but not
    /// persisted as the user's visible message.
    #[serde(default)]
    pub context: Option<String>,
    /// Prior visible messages used to seed a newly branched retry
    /// session. Persisted as hidden system memory by clawd.
    #[serde(default)]
    pub branch_context: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatRequest {
    fn resolve_prompt(&self) -> String {
        if let Some(p) = self.prompt.as_ref().filter(|s| !s.trim().is_empty()) {
            return p.clone();
        }
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default()
    }
}

fn delta_event(text: &str) -> Event {
    Event::default()
        .event("delta")
        .json_data(json!({ "type": "delta", "text": text }))
        .unwrap_or_default()
}

fn error_event(message: &str) -> Event {
    Event::default()
        .event("error")
        .json_data(json!({ "type": "error", "message": message }))
        .unwrap_or_default()
}

fn done_event(envelope: Value) -> Event {
    Event::default()
        .event("done")
        .json_data(envelope)
        .unwrap_or_default()
}

fn json_event(name: &'static str, payload: Value) -> Event {
    Event::default()
        .event(name)
        .json_data(payload)
        .unwrap_or_default()
}

fn visible_tool_progress(progress: &Value) -> Option<(&'static str, Value)> {
    let kind = progress.get("kind").and_then(Value::as_str).unwrap_or("");
    match kind {
        "tool_start" => Some((
            "tool_start",
            json!({
                "kind": kind,
                "id": progress.get("id").cloned().unwrap_or(Value::Null),
                "name": progress.get("name").cloned().unwrap_or(Value::Null),
            }),
        )),
        "tool_result" => Some((
            "tool_result",
            json!({
                "kind": kind,
                "id": progress.get("id").cloned().unwrap_or(Value::Null),
                "name": progress.get("name").cloned().unwrap_or(Value::Null),
                "ok": progress.get("ok").cloned().unwrap_or(Value::Null),
            }),
        )),
        _ => None,
    }
}

/// Translate one persisted clawd stream record into desktop SSE
/// frames. `task.stream` returns records shaped as `{ts, event}` for
/// model events and `{ts, progress}` for runtime tool progress.
fn events_from_stream_record(
    record: &Value,
    turn_emitted_text: &mut bool,
    emitted_any_text: &mut bool,
) -> Vec<Event> {
    if let Some(progress) = record.get("progress") {
        return visible_tool_progress(progress)
            .map(|(name, payload)| vec![json_event(name, payload)])
            .unwrap_or_default();
    }

    let Some(event) = record.get("event") else {
        return Vec::new();
    };
    let kind = event.get("kind").and_then(Value::as_str).unwrap_or("");
    match kind {
        "text_delta" => {
            let text = event.get("text").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                Vec::new()
            } else {
                *turn_emitted_text = true;
                *emitted_any_text = true;
                vec![delta_event(text)]
            }
        }
        "tool_use_start" => vec![json_event(
            "tool_use_start",
            json!({
                "id": event.get("id").cloned().unwrap_or(Value::Null),
                "name": event.get("name").cloned().unwrap_or(Value::Null),
            }),
        )],
        "tool_input_delta" => Vec::new(),
        "tool_use" => vec![json_event(
            "tool_use",
            json!({
                "id": event.get("id").cloned().unwrap_or(Value::Null),
                "name": event.get("name").cloned().unwrap_or(Value::Null),
            }),
        )],
        "message" => {
            let mut frames = Vec::new();
            if !*turn_emitted_text
                && let Some(text) = extract_message_text(event)
                && !text.is_empty()
            {
                *emitted_any_text = true;
                frames.push(delta_event(&text));
            }
            if let Some(calls) = event.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    frames.push(json_event(
                        "tool_use",
                        json!({
                            "id": call.get("id").cloned().unwrap_or(Value::Null),
                            "name": call.get("name").cloned().unwrap_or(Value::Null),
                        }),
                    ));
                }
            }
            frames
        }
        "done" => {
            *turn_emitted_text = false;
            vec![json_event(
                "turn_done",
                json!({
                    "finish": event.get("finish").cloned().unwrap_or(Value::Null),
                    "usage": event.get("usage").cloned().unwrap_or(Value::Null),
                }),
            )]
        }
        "warning" => vec![json_event(
            "warning",
            json!({
                "message": event.get("message").cloned().unwrap_or(Value::Null),
            }),
        )],
        _ => Vec::new(),
    }
}

pub async fn stream_chat(
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let prompt = req.resolve_prompt();
    let session_id = req.session_id.clone();
    let clawd = state.clawd.clone();

    let stream = async_stream::stream! {
        if prompt.trim().is_empty() {
            yield Ok::<_, Infallible>(error_event("empty prompt"));
            return;
        }

        let mut params = json!({ "prompt": prompt });
        if let Some(context) = req.context.as_ref().filter(|value| !value.trim().is_empty()) {
            params["context"] = Value::from(context.trim().to_string());
        }
        if let Some(context) = req
            .branch_context
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            params["branch_context"] = Value::from(context.trim().to_string());
        }
        if let Some(session_id) = session_id.as_ref().filter(|value| !value.trim().is_empty()) {
            params["session_id"] = Value::from(session_id.clone());
        }

        let submitted = match clawd.call(Command::TaskSubmit, params).await {
            Ok(value) => value,
            Err(err) => {
                yield Ok(error_event(&err.to_string()));
                return;
            }
        };

        let task_id = match submitted.get("id").and_then(Value::as_str) {
            Some(id) => id.to_string(),
            None => {
                yield Ok(error_event("clawd task.submit returned no task id"));
                return;
            }
        };
        let submitted_session_id = submitted
            .get("session_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let mut cancel_on_drop = CancelOnDrop::new(clawd.clone(), task_id.clone());
        yield Ok(json_event(
            "task",
            json!({
                "task_id": task_id.clone(),
                "session_id": submitted_session_id,
            }),
        ));

        let mut cursor = 0u64;
        let mut emitted_text = false;
        let mut turn_emitted_text = false;
        let deadline = Instant::now() + MAX_STREAM_DURATION;
        let result = loop {
            if Instant::now() >= deadline {
                yield Ok(error_event("agent task exceeded the maximum stream duration"));
                return;
            }
            let frame = match clawd.call(Command::TaskStream, json!({
                "id": task_id,
                "cursor": cursor,
                "timeout_ms": 1000u64
            })).await {
                Ok(value) => value,
                Err(err) => {
                    yield Ok(error_event(&err.to_string()));
                    return;
                }
            };
            cursor = frame
                .get("cursor")
                .and_then(Value::as_u64)
                .unwrap_or(cursor);
            if let Some(events) = frame.get("events").and_then(Value::as_array) {
                for event in events {
                    for outgoing in events_from_stream_record(
                        event,
                        &mut turn_emitted_text,
                        &mut emitted_text,
                    ) {
                        yield Ok(outgoing);
                    }
                }
            }
            if frame
                .get("terminal")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                break frame.get("job").cloned().unwrap_or(Value::Null);
            }
        };

        let mut payload = result;
        if let Value::Object(ref mut map) = payload {
            if map.get("status").and_then(Value::as_str) == Some("error") {
                let message = map
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("agent task failed");
                yield Ok(error_event(message));
                return;
            }
            if let Some(answer) = map
                .get("response")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
            {
                map.entry("answer".to_string())
                    .or_insert(Value::from(answer.clone()));
                if !emitted_text && !answer.is_empty() {
                    yield Ok(delta_event(&answer));
                }
            }
            map.entry("type".to_string())
                .or_insert(Value::from("done"));
            map.entry("task_id".to_string())
                .or_insert(Value::from(task_id.clone()));
        }

        cancel_on_drop.disarm();
        yield Ok(done_event(payload));
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

pub async fn cancel_chat(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if task_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "task id is required" })),
        ));
    }
    state
        .clawd
        .call(Command::TaskCancel, json!({ "id": task_id }))
        .await
        .map(Json)
        .map_err(|error| {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": error.to_string() })),
            )
        })
}

fn extract_message_text(event: &Value) -> Option<String> {
    let content = event.get("content").and_then(Value::as_array)?;
    let mut text = String::new();
    for block in content {
        if (block.get("type").and_then(Value::as_str) == Some("text")
            || block.get("kind").and_then(Value::as_str) == Some("text"))
            && let Some(chunk) = block.get("text").and_then(Value::as_str)
        {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(chunk);
        }
    }
    if text.is_empty() { None } else { Some(text) }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/routes/chat.rs"
    ));
}
