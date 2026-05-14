//! `POST /api/chat` — Server-Sent Events stream of one chat turn.
//!
//! Wraps `cos agent ask "<prompt>" --stream` as a subprocess and
//! re-frames its output as SSE:
//!
//! * tokens from the agent (written to subprocess stderr) → `delta`
//!   events with payload `{"text": "<chunk>"}`
//! * final JSON envelope (written to subprocess stdout) → `done`
//!   event carrying the full agent response (answer, turns, usage,
//!   tool calls, …)
//! * spawn / IO errors → `error` events
//!
//! The child is killed if the SSE consumer drops the connection.

use std::convert::Infallible;
use std::process::Stdio;

use axum::{
    Json,
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tracing::warn;

use crate::state::AppState;

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ChatRequest {
    /// Single-prompt form used by the simple chat shell.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Multi-turn form for richer clients. We pick the last `user`
    /// message and feed it to `cos agent ask` for now; full history
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

/// Tokio's `Child` does not kill the subprocess on drop. The bridge
/// is a streaming surface — if the HTTP client navigates away we want
/// the agent process to stop too, not pile up zombies.
struct KillOnDrop(Option<Child>);

impl KillOnDrop {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn take(&mut self) -> Option<Child> {
        self.0.take()
    }

    fn stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.0.as_mut().and_then(|c| c.stderr.take())
    }

    fn stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        self.0.as_mut().and_then(|c| c.stdout.take())
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.start_kill();
        }
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
    let cos_bin = state.cos_bin.clone();

    let stream = async_stream::stream! {
        if prompt.trim().is_empty() {
            yield Ok::<_, Infallible>(error_event("empty prompt"));
            return;
        }

        let spawned = Command::new(&cos_bin)
            .args(["agent", "ask"])
            .arg(&prompt)
            .arg("--stream")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let mut guard = match spawned {
            Ok(child) => KillOnDrop::new(child),
            Err(err) => {
                yield Ok(error_event(&format!(
                    "spawn {}: {}",
                    cos_bin.display(),
                    err
                )));
                return;
            }
        };

        let stderr = match guard.stderr() {
            Some(s) => s,
            None => {
                yield Ok(error_event("missing piped stderr"));
                return;
            }
        };
        let stdout = match guard.stdout() {
            Some(s) => s,
            None => {
                yield Ok(error_event("missing piped stdout"));
                return;
            }
        };

        // Drain stdout in the background. The agent writes the final
        // JSON envelope here at the very end; everything before that
        // is just buffered.
        let stdout_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut reader = stdout;
            let _ = reader.read_to_end(&mut buf).await;
            buf
        });

        // Stream stderr → delta events in real time.
        let mut reader = stderr;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]);
                    if chunk.is_empty() {
                        continue;
                    }
                    yield Ok(delta_event(&chunk));
                }
                Err(err) => {
                    warn!(?err, "stderr read failed");
                    break;
                }
            }
        }

        // Reap the child + collect the final JSON envelope.
        let exit = if let Some(mut child) = guard.take() {
            child.wait().await.ok()
        } else {
            None
        };

        let envelope_bytes = stdout_task.await.unwrap_or_default();
        let envelope: Value = match serde_json::from_slice::<Value>(&envelope_bytes) {
            Ok(v) => v,
            Err(_) => {
                let raw = String::from_utf8_lossy(&envelope_bytes).trim().to_string();
                if raw.is_empty() {
                    json!({})
                } else {
                    json!({ "raw_stdout": raw })
                }
            }
        };

        let mut payload = envelope;
        if let Some(status) = exit.and_then(|s| s.code()) {
            if let Value::Object(ref mut map) = payload {
                map.entry("exit_status".to_string())
                    .or_insert(Value::from(status));
            }
        }
        if let Value::Object(ref mut map) = payload {
            map.entry("type".to_string())
                .or_insert(Value::from("done"));
        }

        yield Ok(done_event(payload));
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}
