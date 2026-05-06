//! Media subsystem: voice, TTS, STT, image generation, vision.
//!
//! Phase 5.5 of the migration plan. Each submodule provides:
//!
//!   * A typed `*Request` / `*Response` pair.
//!   * A `Provider` trait that concrete backends (cloud APIs or
//!     local runtimes) implement.
//!   * A `Registry` keyed by name so the runtime can pick a backend
//!     by config string.
//!
//! Concrete cloud providers (Edge / ElevenLabs / OpenAI / Gemini /
//! xAI / MiniMax / Mistral / FAL.ai) and local backends (Piper /
//! KittenTTS / Whisper via `crate::model::tasks::*`) plug in over
//! these traits. This commit ships traits + registries + a
//! `noop` reference impl per surface so the runtime can wire and
//! exercise the call paths before any real provider is configured.

pub mod imagegen;
pub mod imagegen_fal;
pub mod imagegen_openai;
pub mod imagegen_xai;
pub mod stt;
pub mod stt_cloud;
pub mod tts;
pub mod tts_cloud;
pub mod vision;
pub mod voice;

/// Shared error type for the media subsystem. Per-backend errors
/// (HTTP, decode, validation) translate into one of these variants
/// so callers can route uniformly.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("media provider not configured: {0}")]
    NotConfigured(String),

    #[error("invalid media request: {0}")]
    InvalidRequest(String),

    #[error("media provider transport error: {0}")]
    Transport(String),

    #[error("media provider returned error: {status} — {message}")]
    Provider { status: u16, message: String },

    #[error("media response could not be parsed: {0}")]
    Parse(String),

    #[error("media internal: {0}")]
    Internal(String),
}

