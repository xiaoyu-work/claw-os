//! `POST /api/chat` — agentic turn over Server-Sent Events.
//!
//! Request body:
//! ```json
//! { "prompt": "…", "session_id": "…", "use_memory": true, "stream": true }
//! ```
//!
//! Response is `text/event-stream` with the following named events:
//!
//! * `text` — `{ "delta": "…" }` — incremental text delta from the model.
//! * `tool_use_start` — `{ "id": "…", "name": "…" }` — a tool call is forming.
//! * `tool_use` — `{ "id": "…", "name": "…" }` — fully-formed call.
//! * `tool_result` — `{ "id": "…", "name": "…", "ok": bool }`.
//! * `warning` — `{ "message": "…" }`
//! * `evidence` — structural citation binding and confidence metadata.
//! * `done` — `{ "session_id": "…", "model": "…", "turns": N, "answer": "…", "evidence": …, "fallback": …, "finish": "…" }`
//! * `error` — `{ "error": "…" }`

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::stream::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::agent::llm::accumulate::{SinkReady, StreamSink};
use crate::agent::llm::types::StreamEvent;
use crate::agent::runtime;
use crate::agent::runtime::progress::{ProgressReady, ProgressSink};
use crate::agent::runtime::turn_lease::TurnLease;
use crate::agent::web::sse;
use crate::agent::web::state::AppState;

type SseFrame = Result<bytes::Bytes, std::io::Error>;

const SSE_CHANNEL_CAPACITY: usize = 64;
const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

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

    let (session_id, turn_lease) = match begin_turn(&state, req.session_id.as_deref()) {
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
    let interrupt_scope = web_turn_scope(&session_id);

    let (tx, rx) = mpsc::channel::<SseFrame>(SSE_CHANNEL_CAPACITY);

    // Spawn the runtime drive loop on a separate task so the HTTP
    // response stream returns immediately and we can push SSE frames
    // as the model produces them. The response stream owns the join
    // handle and aborts it on drop, so this task cannot outlive a
    // disconnected client.
    let state_cloned = state.clone();
    let prompt = req.prompt.clone();
    let sid_for_task = session_id.clone();
    let scope_for_task = interrupt_scope.clone();
    let use_memory = req.use_memory;
    let drive_task = tokio::spawn(async move {
        if let Err(e) = drive_chat(
            state_cloned,
            prompt,
            sid_for_task.clone(),
            scope_for_task.clone(),
            use_memory,
            tx.clone(),
            turn_lease,
        )
        .await
        {
            let payload = match serde_json::from_str::<serde_json::Value>(&e) {
                Ok(v) => v,
                Err(_) => json!({ "error": e }),
            };
            let _ = send_frame(&tx, &scope_for_task, sse::encode_event("error", &payload)).await;
        }
    });

    let stream = ReceiverStream::new(rx, CancelOnDrop::new(interrupt_scope, drive_task));
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
        .filter(|session_id| !session_id.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    match state.try_acquire_turn(session_id.clone()) {
        Ok(lease) => Ok((session_id, lease)),
        Err(_) => Err(TurnConflict { session_id }),
    }
}

struct TurnConflict {
    session_id: String,
}

fn web_turn_scope(session_id: &str) -> String {
    let conversation = if session_id.len() <= 48
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        session_id.to_string()
    } else {
        format!(
            "sha256-{}",
            &crate::crypto::sha256_hex(session_id.as_bytes())[..24]
        )
    };
    format!("web:{conversation}:turn:{}", uuid::Uuid::new_v4().simple())
}

// ---------------------------------------------------------------------------
// Drive task: builds provider + tools + memory, runs ask_with_stream,
// and pipes events through SseSink (which writes SSE frames into the
// mpsc channel feeding the HTTP body).
// ---------------------------------------------------------------------------

async fn drive_chat(
    state: AppState,
    prompt: String,
    session_id: String,
    interrupt_scope: String,
    use_memory: bool,
    tx: mpsc::Sender<SseFrame>,
    _turn_lease: TurnLease,
) -> Result<(), String> {
    use crate::agent::{memory, setup};

    // Re-read the agent config from disk on every chat request. The
    // server captures a snapshot of `state.inner.cfg` at startup, but
    // long-running daemons keep running across `cos agent setup …`
    // writes (Copilot OAuth, provider switches, …). If we trusted the
    // startup snapshot a daemon launched before the user signed in
    // would forever report "agent not configured" — even after the
    // config file on disk has been fully populated.
    //
    // Falls back to the startup snapshot if disk-read returns an
    // empty provider AND the snapshot has one (paranoia for a config
    // file truncated mid-write).
    let cfg = {
        let fresh = crate::config::intern_user_config().agent.clone();
        if fresh.provider.is_empty() && !state.inner.cfg.provider.is_empty() {
            state.inner.cfg.clone()
        } else {
            fresh
        }
    };
    setup::is_ready(&cfg)?;

    let provider = crate::ai::gate::build_system_provider(&cfg)
        .map_err(|e| format!("provider unavailable: {e}"))?;

    let mut exposure =
        crate::agent::tools::exposure::ToolExposureContext::from_current_session(
            Some(&session_id),
            None,
            crate::agent::tools::exposure::ExecutionHost::Direct,
            runtime::loop_::guardrails_from_cfg(&cfg),
        )?
        .for_authenticated_web_request(state.inner.local_only);
    let mut tools = crate::agent::tools::registry::default_registry();
    tools.set_approval(runtime::loop_::approval_from_cfg(&cfg));
    let _mcp_handles =
        runtime::loop_::attach_mcp_servers_for_cli(&mut tools, &cfg, &mut exposure).await;

    let memory_db = if use_memory {
        match memory::sqlite_fts::MemoryDb::open_default() {
            Ok(db) => Some(db),
            Err(e) => {
                tracing::warn!("web: memory unavailable ({e}); chat will run without history");
                None
            }
        }
    } else {
        None
    };

    let sink_obj = Arc::new(SseSink::new(tx.clone(), interrupt_scope.clone()));
    let sink: Arc<dyn StreamSink> = sink_obj.clone();
    let progress: Arc<dyn ProgressSink> = sink_obj.clone();

    // Announce the session id up front so the client can update its
    // route and show this conversation in the sidebar before the model
    // produces a single token. The `done` event also carries the id at
    // the very end, but emitting `session` early means navigation /
    // refresh / stop mid-stream all preserve the conversation.
    if !send_frame(
        &tx,
        &interrupt_scope,
        sse::encode_event("session", &json!({ "session_id": session_id })),
    )
    .await
    {
        return Ok(());
    }

    // When memory is enabled, replay this session's prior turns into
    // the LLM context so multi-turn chat actually *feels* multi-turn.
    // Without this, `ask_with_stream` seeds `messages` with only the
    // current prompt, so the user's follow-ups arrive context-free
    // and the assistant treats every send like a fresh conversation.
    // Cap replay at 100 rows (≈50 exchanges) to stay well under
    // typical context windows; `ask_with_stream_continuation`
    // truncates long tool-result bodies before replay.
    let result = match memory_db.as_ref() {
        Some(db) => {
            runtime::loop_::ask_with_stream_continuation_scoped_exposure(
                provider.clone(),
                &cfg,
                &prompt,
                None,
                &tools,
                &exposure,
                db,
                &session_id,
                100,
                sink,
                progress,
                &interrupt_scope,
            )
            .await
        }
        None => {
            runtime::loop_::ask_with_stream_scoped_exposure(
                provider.clone(),
                &cfg,
                &prompt,
                None,
                &tools,
                &exposure,
                None,
                sink,
                progress,
                &interrupt_scope,
            )
            .await
        }
    };

    match result {
        Ok(ask) => {
            let finish = sink_obj.snapshot_finish();
            if !send_frame(
                &tx,
                &interrupt_scope,
                sse::encode_event("evidence", &ask.evidence),
            )
            .await
            {
                return Ok(());
            }
            let frame = sse::encode_event(
                "done",
                &json!({
                    "session_id": if ask.session_id.is_empty() { session_id.clone() } else { ask.session_id },
                    "model": ask.model,
                    "provider": ask.provider,
                    "turns": ask.turns,
                    "answer": ask.answer,
                    "evidence": ask.evidence,
                    "fallback": ask.fallback,
                    "finish": finish,
                }),
            );
            let _ = send_frame(&tx, &interrupt_scope, frame).await;
        }
        Err(e) => {
            let frame = sse::encode_event("error", &json!({ "error": e.to_string() }));
            let _ = send_frame(&tx, &interrupt_scope, frame).await;
        }
    }
    Ok(())
}

fn cancel_scope(interrupt_scope: &str) {
    let _ = runtime::interrupt::signal(interrupt_scope);
}

async fn send_frame(tx: &mpsc::Sender<SseFrame>, interrupt_scope: &str, frame: String) -> bool {
    if tx.send(Ok(bytes::Bytes::from(frame))).await.is_ok() {
        true
    } else {
        cancel_scope(interrupt_scope);
        false
    }
}

fn try_send_frame(tx: &mpsc::Sender<SseFrame>, interrupt_scope: &str, frame: String) -> bool {
    match tx.try_send(Ok(bytes::Bytes::from(frame))) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::debug!(
                interrupt_scope,
                capacity = SSE_CHANNEL_CAPACITY,
                "web chat SSE client fell behind; cancelling request"
            );
            cancel_scope(interrupt_scope);
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            cancel_scope(interrupt_scope);
            false
        }
    }
}

/// Stream sink that serializes provider events as SSE frames.
struct SseSink {
    tx: mpsc::Sender<SseFrame>,
    interrupt_scope: String,
    last_finish: Mutex<Option<String>>,
}

impl SseSink {
    fn new(tx: mpsc::Sender<SseFrame>, interrupt_scope: String) -> Self {
        Self {
            tx,
            interrupt_scope,
            last_finish: Mutex::new(None),
        }
    }
    fn snapshot_finish(&self) -> Option<String> {
        self.last_finish.lock().ok().and_then(|g| g.clone())
    }
    fn send(&self, frame: String) {
        let _ = try_send_frame(&self.tx, &self.interrupt_scope, frame);
    }
}

impl StreamSink for SseSink {
    fn wait_ready(&self) -> Option<SinkReady<'_>> {
        Some(Box::pin(async move {
            match self.tx.reserve().await {
                Ok(permit) => {
                    drop(permit);
                    true
                }
                Err(_) => {
                    cancel_scope(&self.interrupt_scope);
                    false
                }
            }
        }))
    }

    fn on_event(&self, event: &StreamEvent) {
        match event {
            StreamEvent::TextDelta { text } => {
                self.send(sse::encode_event("text", &json!({ "delta": text })));
            }
            StreamEvent::ToolUseStart { id, name } => {
                self.send(sse::encode_event(
                    "tool_use_start",
                    &json!({ "id": id, "name": name }),
                ));
            }
            StreamEvent::ToolInputDelta { .. } => {}
            StreamEvent::ToolUse(call) => {
                self.send(sse::encode_event(
                    "tool_use",
                    &json!({
                        "id": call.id,
                        "name": call.name,
                    }),
                ));
            }
            StreamEvent::Reasoning { summary, .. } => {
                if !summary.is_empty() {
                    self.send(sse::encode_event(
                        "reasoning",
                        &json!({ "summary": summary }),
                    ));
                }
            }
            StreamEvent::ToolState { .. } => {}
            StreamEvent::Message(resp) => {
                let mut frames = String::new();
                let mut text = String::new();
                for block in &resp.content {
                    if let crate::agent::llm::types::ContentBlock::Text { text: t } = block {
                        text.push_str(t);
                    }
                }
                if !text.is_empty() {
                    frames.push_str(&sse::encode_event("text", &json!({ "delta": text })));
                }
                for call in &resp.tool_calls {
                    frames.push_str(&sse::encode_event(
                        "tool_use",
                        &json!({
                            "id": call.id,
                            "name": call.name,
                        }),
                    ));
                }
                if !frames.is_empty() {
                    self.send(frames);
                }
            }
            StreamEvent::Done { finish, usage } => {
                let finish_str = format!("{finish:?}").to_ascii_lowercase();
                if let Ok(mut g) = self.last_finish.lock() {
                    *g = Some(finish_str.clone());
                }
                self.send(sse::encode_event(
                    "turn_done",
                    &json!({
                        "finish": finish_str,
                        "usage": {
                            "input_tokens": usage.input_tokens,
                            "output_tokens": usage.output_tokens,
                            "cache_read_tokens": usage.cache_read_tokens,
                            "cache_write_tokens": usage.cache_write_tokens,
                        }
                    }),
                ));
            }
            StreamEvent::Warning { message } => {
                self.send(sse::encode_event("warning", &json!({ "message": message })));
            }
        }
    }
}

impl ProgressSink for SseSink {
    fn wait_ready(&self) -> Option<ProgressReady<'_>> {
        <Self as StreamSink>::wait_ready(self)
    }

    fn on_tool_start(&self, id: &str, name: &str, _input: &Value) {
        self.send(sse::encode_event(
            "tool_start",
            &json!({ "id": id, "name": name }),
        ));
    }
    fn on_tool_result(
        &self,
        id: &str,
        name: &str,
        ok: bool,
        _latency_ms: u64,
        _bytes_returned: usize,
        _content_preview: &str,
    ) {
        self.send(sse::encode_event(
            "tool_result",
            &json!({
                "id": id,
                "name": name,
                "ok": ok,
            }),
        ));
    }
}

// ---------------------------------------------------------------------------
// Stream adapter: wraps the bounded receiver as a futures Stream and
// owns the runtime task for exactly as long as axum owns the body.
// ---------------------------------------------------------------------------

struct CancelOnDrop {
    interrupt_scope: String,
    task: tokio::task::JoinHandle<()>,
}

impl CancelOnDrop {
    fn new(interrupt_scope: String, task: tokio::task::JoinHandle<()>) -> Self {
        Self {
            interrupt_scope,
            task,
        }
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        cancel_scope(&self.interrupt_scope);
        self.task.abort();
    }
}

struct ReceiverStream {
    rx: mpsc::Receiver<SseFrame>,
    heartbeat: tokio::time::Interval,
    _cancel: CancelOnDrop,
}

impl ReceiverStream {
    fn new(rx: mpsc::Receiver<SseFrame>, cancel: CancelOnDrop) -> Self {
        let start = tokio::time::Instant::now() + HEARTBEAT_INTERVAL;
        let mut heartbeat = tokio::time::interval_at(start, HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self {
            rx,
            heartbeat,
            _cancel: cancel,
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
                    std::task::Poll::Ready(_) => {
                        let ping = bytes::Bytes::from(sse::encode_comment("ping"));
                        std::task::Poll::Ready(Some(Ok(ping)))
                    }
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
