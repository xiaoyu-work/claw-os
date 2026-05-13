//! `POST /api/chat` — Server-Sent Events stream of one chat turn.
//!
//! Stub today: yields a couple of canned deltas + a done event so
//! the React UI can develop against the protocol. Wired to a real
//! `cos agent stream` subprocess in the `bridge-http-server` todo.

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    Json,
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

pub async fn stream_chat(
    State(_state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let last = req
        .messages
        .last()
        .map(|m| m.content.clone())
        .unwrap_or_default();

    let stream = async_stream::stream! {
        let preview = if last.is_empty() { "(empty)".to_string() } else { last };
        let chunks = [
            format!("(stub) heard: {preview}\n"),
            "wiring to `cos agent stream` next.".to_string(),
        ];
        for chunk in chunks {
            tokio::time::sleep(Duration::from_millis(40)).await;
            yield Ok::<_, Infallible>(
                Event::default().event("delta").json_data(serde_json::json!({
                    "type": "delta",
                    "text": chunk,
                })).unwrap_or_default()
            );
        }
        yield Ok(Event::default().event("done").json_data(serde_json::json!({
            "type": "done",
            "finish_reason": "stop",
        })).unwrap_or_default());
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}
