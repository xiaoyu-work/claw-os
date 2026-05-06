//! Standard task surface exposed to callers (agent/memory/media subsystems).
//!
//! Each task module presents a stable Rust API and routes internally to the
//! correct engine (`engines::ort` or `engines::llama`).

pub mod embed;
pub mod imagegen;
pub mod llm;
pub mod qwen3_genai;
pub mod stt;
pub mod tts;
pub mod vision;
