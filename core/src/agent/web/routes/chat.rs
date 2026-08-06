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
//! * `tool_use` — `{ "id": "…", "name": "…", "input": …json }` — fully-formed call.
//! * `tool_result` — `{ "id": "…", "name": "…", "ok": bool, "latency_ms": N, "bytes": N, "preview": "…" }`
//! * `warning` — `{ "message": "…" }`
//! * `evidence` — structural citation binding and confidence metadata.
//! * `done` — `{ "session_id": "…", "model": "…", "turns": N, "answer": "…", "evidence": …, "finish": "…" }`
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

use crate::agent::llm::accumulate::StreamSink;
use crate::agent::llm::types::StreamEvent;
use crate::agent::runtime;
use crate::agent::runtime::progress::ProgressSink;
use crate::agent::web::sse;
use crate::agent::web::state::AppState;

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

pub async fn handler(
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Response {
    if req.prompt.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"empty prompt"}"#,
        )
            .into_response();
    }

    let session_id = req
        .session_id
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let (tx, rx) = mpsc::unbounded_channel::<Result<bytes::Bytes, std::io::Error>>();
    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();

    // Spawn the runtime drive loop on a separate task so the HTTP
    // response stream returns immediately and we can push SSE frames
    // as the model produces them.
    let state_cloned = state.clone();
    let prompt = req.prompt.clone();
    let sid_for_task = session_id.clone();
    let use_memory = req.use_memory;
    let tx_for_drive = tx.clone();

    tokio::spawn(async move {
        if let Err(e) =
            drive_chat(state_cloned, prompt, sid_for_task, use_memory, tx_for_drive.clone()).await
        {
            let payload = match serde_json::from_str::<serde_json::Value>(&e) {
                Ok(v) => v,
                Err(_) => json!({ "error": e }),
            };
            let frame = sse::encode_event("error", &payload);
            let _ = tx_for_drive.send(Ok(bytes::Bytes::from(frame)));
        }
        // Signal heartbeat to stop. The drive task's sender is
        // dropped here, but heartbeat keeps the receiver alive
        // through its own sender clone.
        let _ = done_tx.send(());
    });

    // Heartbeat task: emit an SSE comment every 15s so the connection
    // stays open through aggressive idle-timeout proxies.
    let tx_hb = tx.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if tx_hb
                        .send(Ok(bytes::Bytes::from(sse::encode_comment("ping"))))
                        .is_err()
                    {
                        break;
                    }
                }
                _ = &mut done_rx => break,
            }
        }
    });

    // Original `tx` is dropped here; the channel stays open via the
    // clones held by the drive and heartbeat tasks. When both finish
    // the receiver sees end-of-stream and the SSE body closes.
    drop(tx);

    let stream = ReceiverStream::new(rx);
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

// ---------------------------------------------------------------------------
// Drive task: builds provider + tools + memory, runs ask_with_stream,
// and pipes events through SseSink (which writes SSE frames into the
// mpsc channel feeding the HTTP body).
// ---------------------------------------------------------------------------

async fn drive_chat(
    state: AppState,
    prompt: String,
    session_id: String,
    use_memory: bool,
    tx: mpsc::UnboundedSender<Result<bytes::Bytes, std::io::Error>>,
) -> Result<(), String> {
    use crate::agent::{llm, memory, setup};

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

    let provider = llm::registry::build(&cfg.provider, &cfg.model, &cfg)
        .map_err(|e| format!("provider unavailable: {e}"))?;
    let provider = crate::ai::gate::wrap_for_system(provider);

    let mut tools = crate::agent::tools::registry::default_registry();
    tools.set_guardrails(runtime::loop_::guardrails_from_cfg(&cfg));
    tools.set_approval(runtime::loop_::approval_from_cfg(&cfg));
    let _mcp_handles = runtime::loop_::attach_mcp_servers_for_cli(&mut tools, &cfg).await;

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

    let sink_obj = Arc::new(SseSink::new(tx.clone()));
    let sink: Arc<dyn StreamSink> = sink_obj.clone();
    let progress: Arc<dyn ProgressSink> = sink_obj.clone();

    // Announce the session id up front so the client can update its
    // route and show this conversation in the sidebar before the model
    // produces a single token. The `done` event also carries the id at
    // the very end, but emitting `session` early means navigation /
    // refresh / stop mid-stream all preserve the conversation.
    let _ = tx.send(Ok(bytes::Bytes::from(sse::encode_event(
        "session",
        &json!({ "session_id": session_id }),
    ))));

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
            runtime::loop_::ask_with_stream_continuation(
                provider.clone(),
                &cfg,
                &prompt,
                &tools,
                db,
                &session_id,
                100,
                sink,
                progress,
            )
            .await
        }
        None => {
            runtime::loop_::ask_with_stream(
                provider.clone(),
                &cfg,
                &prompt,
                &tools,
                None,
                sink,
                progress,
            )
            .await
        }
    };

    match result {
        Ok(ask) => {
            let finish = sink_obj.snapshot_finish();
            let _ = tx.send(Ok(bytes::Bytes::from(sse::encode_event(
                "evidence",
                &ask.evidence,
            ))));
            let frame = sse::encode_event(
                "done",
                &json!({
                    "session_id": if ask.session_id.is_empty() { session_id.clone() } else { ask.session_id },
                    "model": ask.model,
                    "provider": ask.provider,
                    "turns": ask.turns,
                    "answer": ask.answer,
                    "evidence": ask.evidence,
                    "finish": finish,
                }),
            );
            let _ = tx.send(Ok(bytes::Bytes::from(frame)));
        }
        Err(e) => {
            let frame = sse::encode_event("error", &json!({ "error": e.to_string() }));
            let _ = tx.send(Ok(bytes::Bytes::from(frame)));
        }
    }
    Ok(())
}

/// Stream sink that serializes provider events as SSE frames.
struct SseSink {
    tx: mpsc::UnboundedSender<Result<bytes::Bytes, std::io::Error>>,
    last_finish: Mutex<Option<String>>,
}

impl SseSink {
    fn new(tx: mpsc::UnboundedSender<Result<bytes::Bytes, std::io::Error>>) -> Self {
        Self {
            tx,
            last_finish: Mutex::new(None),
        }
    }
    fn snapshot_finish(&self) -> Option<String> {
        self.last_finish.lock().ok().and_then(|g| g.clone())
    }
    fn send(&self, frame: String) {
        let _ = self.tx.send(Ok(bytes::Bytes::from(frame)));
    }
}

impl StreamSink for SseSink {
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
            StreamEvent::ToolInputDelta { id, partial_json } => {
                self.send(sse::encode_event(
                    "tool_input_delta",
                    &json!({ "id": id, "partial": partial_json }),
                ));
            }
            StreamEvent::ToolUse(call) => {
                self.send(sse::encode_event(
                    "tool_use",
                    &json!({
                        "id": call.id,
                        "name": call.name,
                        "input": call.input,
                    }),
                ));
            }
            StreamEvent::Message(resp) => {
                let mut text = String::new();
                for block in &resp.content {
                    if let crate::agent::llm::types::ContentBlock::Text { text: t } = block {
                        text.push_str(t);
                    }
                }
                if !text.is_empty() {
                    self.send(sse::encode_event("text", &json!({ "delta": text })));
                }
                for call in &resp.tool_calls {
                    self.send(sse::encode_event(
                        "tool_use",
                        &json!({
                            "id": call.id,
                            "name": call.name,
                            "input": call.input,
                        }),
                    ));
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
                self.send(sse::encode_event(
                    "warning",
                    &json!({ "message": message }),
                ));
            }
        }
    }
}

impl ProgressSink for SseSink {
    fn on_tool_start(&self, id: &str, name: &str, input: &Value) {
        self.send(sse::encode_event(
            "tool_start",
            &json!({ "id": id, "name": name, "input": input }),
        ));
    }
    fn on_tool_result(
        &self,
        id: &str,
        name: &str,
        ok: bool,
        latency_ms: u64,
        bytes_returned: usize,
        content_preview: &str,
    ) {
        self.send(sse::encode_event(
            "tool_result",
            &json!({
                "id": id,
                "name": name,
                "ok": ok,
                "latency_ms": latency_ms,
                "bytes": bytes_returned,
                "preview": content_preview,
            }),
        ));
    }
}

// ---------------------------------------------------------------------------
// Stream adapter: wraps tokio mpsc::UnboundedReceiver as a futures
// Stream of byte chunks suitable for axum's Body::from_stream.
// ---------------------------------------------------------------------------

struct ReceiverStream<T> {
    rx: mpsc::UnboundedReceiver<T>,
}

impl<T> ReceiverStream<T> {
    fn new(rx: mpsc::UnboundedReceiver<T>) -> Self {
        Self { rx }
    }
}

impl<T> Stream for ReceiverStream<T> {
    type Item = T;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}
