//! `POST /api/voice/upload` — accept a recorded audio blob.
//!
//! Stub: discards the body and returns a placeholder transcript.
//! The actual STT model wiring is intentionally deferred per Phase 8
//! plan (`voice-ui-hooks` todo).

use axum::{Json, body::Bytes, extract::State};
use serde::Serialize;

use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct VoiceResponse {
    pub transcript: String,
    pub bytes_received: usize,
}

pub async fn upload(State(_state): State<AppState>, body: Bytes) -> Json<VoiceResponse> {
    Json(VoiceResponse {
        transcript: "(voice model not wired yet — placeholder transcript)".into(),
        bytes_received: body.len(),
    })
}
