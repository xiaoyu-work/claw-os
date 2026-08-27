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
    extract::{Path, State, rejection::JsonRejection},
    response::sse::{Event, KeepAlive, Sse},
};
use cos_agent_protocol::{
    CancelResponse, ChatRequest, DeltaPayload, ErrorCode, StreamError, StreamEvent,
};
use futures::stream::Stream;
use serde_json::{Value, json};

use crate::{api_error::ApiError, state::AppState, translation};
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

fn delta_event(text: &str) -> Event {
    protocol_event(StreamEvent::Delta(DeltaPayload::new(text)))
}

fn error_event(message: &str) -> Event {
    protocol_event(StreamEvent::Error(StreamError::new(message)))
}

fn protocol_event(event: StreamEvent) -> Event {
    let name = event.event_name();
    let data = event.to_json().unwrap_or_else(|_| {
        r#"{"type":"error","message":"failed to serialize stream event"}"#.to_string()
    });
    Event::default().event(name).data(data)
}

pub async fn stream_chat(
    State(state): State<AppState>,
    request: Result<Json<ChatRequest>, JsonRejection>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let Json(req) = request.map_err(|_| {
        ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            ErrorCode::InvalidRequest,
            "invalid chat request",
        )
    })?;
    let prompt = req.resolved_prompt();
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

        let started = match translation::task_started(submitted) {
            Ok(started) => started,
            Err(error) => {
                yield Ok(error_event(&error));
                return;
            }
        };
        let task_id = started.task_id.clone();
        let mut cancel_on_drop = CancelOnDrop::new(clawd.clone(), task_id.clone());
        yield Ok(protocol_event(StreamEvent::TaskStarted(started)));

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
            let frame = match translation::task_stream(frame) {
                Ok(frame) => frame,
                Err(error) => {
                    yield Ok(error_event(&error));
                    return;
                }
            };
            cursor = frame.cursor;
            for record in frame.events {
                for outgoing in translation::stream_events(
                    record,
                    &mut turn_emitted_text,
                    &mut emitted_text,
                ) {
                    yield Ok(protocol_event(outgoing));
                }
            }
            if frame.terminal {
                break frame.job;
            }
        };

        let payload = match result.into_done() {
            Ok(payload) => payload,
            Err(error) => {
                yield Ok(error_event(&error));
                return;
            }
        };
        if let Some(answer) = payload.presented_answer()
            && !emitted_text
        {
            yield Ok(delta_event(&answer));
        }

        cancel_on_drop.disarm();
        yield Ok(protocol_event(StreamEvent::Done(payload)));
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

pub async fn cancel_chat(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<CancelResponse>, ApiError> {
    if task_id.trim().is_empty() {
        return Err(ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            ErrorCode::InvalidRequest,
            "task id is required",
        ));
    }
    let value = state
        .clawd
        .call(Command::TaskCancel, json!({ "id": task_id }))
        .await
        .map_err(|error| ApiError::bad_gateway(error.to_string()))?;
    translation::cancel_response(value)
        .map(Json)
        .map_err(ApiError::bad_gateway)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/routes/chat.rs"
    ));
}
