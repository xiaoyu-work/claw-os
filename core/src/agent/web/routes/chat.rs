//! `POST /api/chat` — a durable `clawd` task projected as Web SSE.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::stream::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::agent::llm::types::ContentBlock;
use crate::agent::llm::StreamEvent;
use crate::agent::runtime::turn_lease::TurnLease;
use crate::agent::web::sse;
use crate::agent::web::state::AppState;
use crate::clawd::routes::Command;

type SseFrame = Result<bytes::Bytes, std::io::Error>;

const SSE_CHANNEL_CAPACITY: usize = 64;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const MAX_STREAM_DURATION: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub prompt: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default = "default_true")]
    pub use_memory: bool,
}

fn default_true() -> bool {
    true
}

pub async fn handler(State(state): State<AppState>, Json(req): Json<ChatRequest>) -> Response {
    if req.prompt.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"empty prompt"}"#,
        )
            .into_response();
    }

    let requested_session = req
        .session_id
        .as_deref()
        .filter(|session_id| !session_id.trim().is_empty())
        .map(str::to_string);
    let (lease_key, turn_lease) = match begin_turn(&state, requested_session.as_deref()) {
        Ok(turn) => turn,
        Err(conflict) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "session already has an active turn",
                    "code": "session_busy",
                    "session_id": conflict.session_id,
                })),
            )
                .into_response();
        }
    };

    let (tx, rx) = mpsc::channel::<SseFrame>(SSE_CHANNEL_CAPACITY);
    let disconnected = Arc::new(AtomicBool::new(false));
    let drive_disconnected = disconnected.clone();
    let drive_state = state.clone();
    let drive_task = tokio::spawn(async move {
        if let Err(error) = drive_chat(
            drive_state,
            req.prompt,
            requested_session,
            lease_key,
            req.use_memory,
            tx.clone(),
            drive_disconnected,
            turn_lease,
        )
        .await
        {
            let _ = tx
                .send(Ok(bytes::Bytes::from(sse::encode_event(
                    "error",
                    &json!({ "error": error }),
                ))))
                .await;
        }
    });

    let stream = ReceiverStream::new(rx, DisconnectOnDrop::new(disconnected, drive_task));
    let body = Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header("X-Accel-Buffering", "no")
        .body(body)
        .unwrap_or_else(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "stream build failed").into_response()
        })
}

fn begin_turn(
    state: &AppState,
    requested_session_id: Option<&str>,
) -> Result<(String, TurnLease), TurnConflict> {
    let session_id = requested_session_id
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    match state.try_acquire_turn(session_id.clone()) {
        Ok(lease) => Ok((session_id, lease)),
        Err(_) => Err(TurnConflict { session_id }),
    }
}

#[derive(Debug)]
struct TurnConflict {
    session_id: String,
}

async fn drive_chat(
    state: AppState,
    prompt: String,
    requested_session: Option<String>,
    provisional_session: String,
    use_memory: bool,
    tx: mpsc::Sender<SseFrame>,
    disconnected: Arc<AtomicBool>,
    turn_lease: TurnLease,
) -> Result<(), String> {
    let mut turn_lease = Some(turn_lease);
    if requested_session
        .as_deref()
        .is_some_and(|session_id| session_id.parse::<crate::session::SessionId>().is_err())
    {
        return Err(
            "This pre-queue conversation is read-only. Start a new chat to continue with durable tasks."
                .to_string(),
        );
    }
    let mut params = json!({ "prompt": prompt, "use_memory": use_memory });
    if let Some(session_id) = requested_session {
        params["session_id"] = json!(session_id);
    }
    let submitted = super::clawd::request(Command::TaskSubmit, params)
        .await
        .map_err(|error| format!("task submission failed: {}", error.message()))?;
    let task_id = required_field(&submitted, "id")?.to_string();
    let session_id = submitted
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or(&provisional_session)
        .to_string();
    if session_id != provisional_session {
        let actual_lease = match state.try_acquire_turn(session_id.clone()) {
            Ok(lease) => lease,
            Err(_) => {
                cancel_task(&task_id).await;
                return Err("clawd assigned a session that already has an active turn".to_string());
            }
        };
        turn_lease = Some(actual_lease);
    }

    if disconnected.load(Ordering::SeqCst) {
        return Ok(());
    }
    if !send_frame(
        &tx,
        sse::encode_event(
            "task",
            &json!({ "task_id": task_id, "session_id": session_id }),
        ),
    )
    .await
        || !send_frame(
            &tx,
            sse::encode_event("session", &json!({ "session_id": session_id })),
        )
        .await
    {
        return Ok(());
    }

    let mut cursor = 0u64;
    let mut emitted_text = false;
    let mut turn_emitted_text = false;
    let mut last_finish: Option<String> = None;
    let deadline = Instant::now() + MAX_STREAM_DURATION;
    let final_job = loop {
        if disconnected.load(Ordering::SeqCst) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(
                "live stream ended after 30 minutes; the durable task is still available in Tasks"
                    .to_string(),
            );
        }
        let frame = super::clawd::request(
            Command::TaskStream,
            json!({
                "id": task_id,
                "cursor": cursor,
                "timeout_ms": 1_000u64,
            }),
        )
        .await
        .map_err(|error| format!("task stream failed: {}", error.message()))?;
        cursor = frame
            .get("cursor")
            .and_then(Value::as_u64)
            .ok_or_else(|| "task stream returned no cursor".to_string())?;
        for record in frame
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            for outgoing in project_stream_record(
                record,
                &mut turn_emitted_text,
                &mut emitted_text,
                &mut last_finish,
            )? {
                if !send_frame(&tx, outgoing).await {
                    return Ok(());
                }
            }
        }
        if frame
            .get("terminal")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            break frame
                .get("job")
                .cloned()
                .ok_or_else(|| "terminal task stream returned no job".to_string())?;
        }
    };

    match final_job.get("status").and_then(Value::as_str) {
        Some("ok") => {}
        Some("cancelled") => return Err("agent task was cancelled".to_string()),
        Some("error") => {
            return Err(final_job
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("agent task failed")
                .to_string());
        }
        Some(status) => return Err(format!("agent task ended in unexpected state: {status}")),
        None => return Err("terminal task has no status".to_string()),
    }

    let answer = final_job
        .get("response")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !emitted_text && !answer.is_empty() && !send_frame(&tx, text_frame(answer)).await {
        return Ok(());
    }
    if let Some(evidence) = final_job.get("evidence").filter(|value| !value.is_null()) {
        if !send_frame(&tx, sse::encode_event("evidence", evidence)).await {
            return Ok(());
        }
    }
    let done = json!({
        "session_id": final_job
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or(&session_id),
        "model": final_job.get("model").cloned().unwrap_or(Value::Null),
        "provider": final_job.get("provider").cloned().unwrap_or(Value::Null),
        "turns": final_job.get("turns_used").cloned().unwrap_or(Value::Null),
        "answer": final_job.get("response").cloned().unwrap_or(Value::Null),
        "evidence": final_job.get("evidence").cloned().unwrap_or(Value::Null),
        "fallback": final_job.get("fallback").cloned().unwrap_or(Value::Null),
        "finish": last_finish,
    });
    let _ = send_frame(&tx, sse::encode_event("done", &done)).await;
    drop(turn_lease);
    Ok(())
}

fn project_stream_record(
    record: Value,
    turn_emitted_text: &mut bool,
    emitted_text: &mut bool,
    last_finish: &mut Option<String>,
) -> Result<Vec<String>, String> {
    if let Some(progress) = record.get("progress") {
        return Ok(project_progress(progress));
    }
    let Some(event) = record.get("event") else {
        return Ok(Vec::new());
    };
    let event = match serde_json::from_value::<StreamEvent>(event.clone()) {
        Ok(event) => event,
        Err(error) => {
            tracing::warn!(%error, "skipping unsupported task stream event");
            return Ok(Vec::new());
        }
    };
    Ok(match event {
        StreamEvent::TextDelta { text } => {
            *turn_emitted_text = true;
            *emitted_text = true;
            vec![text_frame(&text)]
        }
        StreamEvent::ToolUseStart { id, name } => vec![sse::encode_event(
            "tool_use_start",
            &json!({ "id": id, "name": name }),
        )],
        StreamEvent::ToolInputDelta { .. } | StreamEvent::ToolState { .. } => Vec::new(),
        StreamEvent::ToolUse(call) => vec![sse::encode_event(
            "tool_use",
            &json!({ "id": call.id, "name": call.name }),
        )],
        StreamEvent::Reasoning { summary, .. } if !summary.is_empty() => vec![sse::encode_event(
            "reasoning",
            &json!({ "summary": summary }),
        )],
        StreamEvent::Reasoning { .. } => Vec::new(),
        StreamEvent::Message(response) => {
            let mut frames = Vec::new();
            if !*turn_emitted_text {
                let text = response
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                if !text.is_empty() {
                    *emitted_text = true;
                    frames.push(text_frame(&text));
                }
            }
            frames.extend(response.tool_calls.into_iter().map(|call| {
                sse::encode_event("tool_use", &json!({ "id": call.id, "name": call.name }))
            }));
            frames
        }
        StreamEvent::Done { finish, usage } => {
            let finish = format!("{finish:?}").to_ascii_lowercase();
            *last_finish = Some(finish.clone());
            *turn_emitted_text = false;
            vec![sse::encode_event(
                "turn_done",
                &json!({
                    "finish": finish,
                    "usage": {
                        "input_tokens": usage.input_tokens,
                        "output_tokens": usage.output_tokens,
                        "cache_read_tokens": usage.cache_read_tokens,
                        "cache_write_tokens": usage.cache_write_tokens,
                    },
                }),
            )]
        }
        StreamEvent::Warning { message } => vec![warning_frame(&message)],
    })
}

fn project_progress(progress: &Value) -> Vec<String> {
    let id = progress
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let name = progress
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match progress.get("kind").and_then(Value::as_str) {
        Some("tool_start") => vec![sse::encode_event(
            "tool_start",
            &json!({ "id": id, "name": name }),
        )],
        Some("tool_result") => vec![sse::encode_event(
            "tool_result",
            &json!({
                "id": id,
                "name": name,
                "ok": progress.get("ok").and_then(Value::as_bool).unwrap_or(false),
            }),
        )],
        Some("waiting_approval") => {
            let ids = progress
                .get("request_ids")
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            vec![warning_frame(&format!("Waiting for approval: {ids}"))]
        }
        Some("approval_resumed") => vec![warning_frame(
            "Approval granted. The task is resuming automatically.",
        )],
        _ => Vec::new(),
    }
}

fn text_frame(text: &str) -> String {
    sse::encode_event("text", &json!({ "delta": text }))
}

fn warning_frame(message: &str) -> String {
    sse::encode_event("warning", &json!({ "message": message }))
}

fn required_field<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("task response is missing {key}"))
}

async fn cancel_task(task_id: &str) {
    if let Err(error) = super::clawd::request(Command::TaskCancel, json!({ "id": task_id })).await {
        tracing::warn!(task = task_id, error = %error.message(), "failed to cancel Web task");
    }
}

async fn send_frame(tx: &mpsc::Sender<SseFrame>, frame: String) -> bool {
    tx.send(Ok(bytes::Bytes::from(frame))).await.is_ok()
}

struct DisconnectOnDrop {
    disconnected: Arc<AtomicBool>,
    _task: tokio::task::JoinHandle<()>,
}

impl DisconnectOnDrop {
    fn new(disconnected: Arc<AtomicBool>, task: tokio::task::JoinHandle<()>) -> Self {
        Self {
            disconnected,
            _task: task,
        }
    }
}

impl Drop for DisconnectOnDrop {
    fn drop(&mut self) {
        self.disconnected.store(true, Ordering::SeqCst);
    }
}

struct ReceiverStream {
    rx: mpsc::Receiver<SseFrame>,
    heartbeat: tokio::time::Interval,
    _disconnect: DisconnectOnDrop,
}

impl ReceiverStream {
    fn new(rx: mpsc::Receiver<SseFrame>, disconnect: DisconnectOnDrop) -> Self {
        let start = tokio::time::Instant::now() + HEARTBEAT_INTERVAL;
        let mut heartbeat = tokio::time::interval_at(start, HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self {
            rx,
            heartbeat,
            _disconnect: disconnect,
        }
    }
}

impl Stream for ReceiverStream {
    type Item = SseFrame;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.rx.poll_recv(cx) {
            std::task::Poll::Ready(item) => std::task::Poll::Ready(item),
            std::task::Poll::Pending => {
                match std::pin::Pin::new(&mut this.heartbeat).poll_tick(cx) {
                    std::task::Poll::Ready(_) => std::task::Poll::Ready(Some(Ok(
                        bytes::Bytes::from(sse::encode_comment("ping")),
                    ))),
                    std::task::Poll::Pending => std::task::Poll::Pending,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/web/routes/chat.rs"
    ));
}
