//! Inference engines hosted in the model-runtime daemon.
//!
//!   - `ort`   — ONNX Runtime singleton (STT/TTS/Embedding/Vision/ImageGen)
//!   - `llama` — llama.cpp singleton (LLM, GGUF)
//!
//! Phase 0.5 defines the trait. Concrete impls land when first model files arrive.

use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("engine not linked: {0}")]
    NotLinked(&'static str),
    #[error("model not loaded: {0}")]
    NotLoaded(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// What an engine implementation must offer.
#[async_trait]
pub trait Engine: Send + Sync {
    fn name(&self) -> &'static str;
    fn supports(&self, format: super::registry::Format) -> bool;

    async fn load(&self, manifest: &super::registry::Manifest) -> Result<()>;
    async fn unload(&self, name: &str) -> Result<()>;
    async fn is_loaded(&self, name: &str) -> bool;
}
