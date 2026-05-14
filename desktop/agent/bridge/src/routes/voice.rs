//! `POST /api/voice/upload` — accept a recorded audio blob.
//!
//! The web client sends the raw audio bytes (Content-Type carries the
//! mime, e.g. `audio/webm`); we forward them to a future STT backend
//! and return a single `{text}` field that the React composer drops
//! into the chat input.
//!
//! Today the STT path is a stub: we record what we received and
//! return a placeholder transcript so the round-trip + UI plumbing
//! can be exercised end-to-end. Real model wiring is tracked in a
//! follow-up.

use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, header},
};
use serde::Serialize;

use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct VoiceResponse {
    /// Transcript text — drops into the React input on success.
    pub text: String,
    pub bytes_received: usize,
    pub mime_type: String,
    /// Set when no real STT model is configured; the UI shows it as
    /// an inline tip.
    pub placeholder: bool,
}

pub async fn upload(
    State(_state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Json<VoiceResponse> {
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    Json(VoiceResponse {
        text: format!(
            "[voice transcript placeholder — received {} bytes of {mime}]",
            body.len()
        ),
        bytes_received: body.len(),
        mime_type: mime,
        placeholder: true,
    })
}
