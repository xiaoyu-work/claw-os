//! `POST /api/chat` — Server-Sent Events stream of one chat turn.
//!
//! Submits the prompt to `clawd` and re-frames the daemon response as SSE:
//!
//! * final answer text from `clawd` → `delta`
//! * final task envelope from `clawd` → `done`
//!   event carrying the full agent response (answer, turns, usage,
//!   tool calls, …)
//! * daemon / IO errors → `error` events

use std::convert::Infallible;
use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::clawd;
use crate::state::AppState;

/// Hard ceiling on a single chat turn. `task.stream` blocks ~1s per poll, so
/// without this a task that never reports `terminal` (stuck agent, or a frame
/// that never advances the cursor) would keep the SSE connection open and
/// re-poll clawd once a second indefinitely while the client stays connected.
const MAX_STREAM_DURATION: Duration = Duration::from_secs(30 * 60);

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

pub async fn stream_chat(
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let prompt = req.resolve_prompt();
    let session_id = req.session_id.clone();
    let socket = state.clawd_socket.clone();

    let stream = async_stream::stream! {
        if prompt.trim().is_empty() {
            yield Ok::<_, Infallible>(error_event("empty prompt"));
            return;
        }

        let mut params = json!({ "prompt": prompt });
        if let Some(session_id) = session_id.as_ref().filter(|value| !value.trim().is_empty()) {
            params["session_id"] = Value::from(session_id.clone());
        }

        let submitted = match clawd::request(&socket, "task.submit", params).await {
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

        let mut cursor = 0u64;
        let mut emitted_text = false;
        let deadline = Instant::now() + MAX_STREAM_DURATION;
        let result = loop {
            if Instant::now() >= deadline {
                yield Ok(error_event("agent task exceeded the maximum stream duration"));
                return;
            }
            let frame = match clawd::request(&socket, "task.stream", json!({
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
                    if let Some(text) = text_from_stream_event(event) {
                        if !text.is_empty() {
                            emitted_text = true;
                            yield Ok(delta_event(&text));
                        }
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
        }

        yield Ok(done_event(payload));
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn text_from_stream_event(frame: &Value) -> Option<String> {
    let event = frame.get("event")?;
    match event.get("kind").and_then(Value::as_str)? {
        "text_delta" => event
            .get("text")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        "message" => extract_message_text(event),
        _ => None,
    }
}

fn extract_message_text(event: &Value) -> Option<String> {
    let content = event.get("content").and_then(Value::as_array)?;
    let mut text = String::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) == Some("text")
            || block.get("kind").and_then(Value::as_str) == Some("text")
        {
            if let Some(chunk) = block.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(chunk);
            }
        }
    }
    if text.is_empty() { None } else { Some(text) }
}
