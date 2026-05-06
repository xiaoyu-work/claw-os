//! Inference engines hosted in the model-runtime daemon.
//!
//!   - `ort`   — ONNX Runtime singleton (STT/TTS/Embedding/Vision/ImageGen)
//!   - `llama_cpp` — llama.cpp singleton (LLM, GGUF), gated behind the
//!                   `llama_cpp` cargo feature.
//!
//! Phase 0.5 defines the trait and ships the llama_cpp scaffolding (FFI +
//! safe wrapper). Concrete inference is wired when the user supplies the
//! first GGUF file.

use async_trait::async_trait;

pub mod llama_cpp;

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // Variants surface only behind `cfg(feature = "llama_cpp")`.
pub enum EngineError {
    #[error("engine not linked: {0}")]
    NotLinked(&'static str),
    #[error("model not loaded: {0}")]
    NotLoaded(String),
    #[error("model load failed: {0}")]
    LoadFailed(String),
    #[error("inference failed: {0}")]
    InferenceFailed(String),
    #[error("invalid model path: {0}")]
    InvalidModelPath(String),
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

/// Names of every engine compiled into this binary. Used by `cos model
/// status` and the agent provider registry to decide whether local
/// inference is available.
pub fn engines_linked() -> Vec<&'static str> {
    let mut out = Vec::new();
    if llama_cpp::IS_LINKED {
        out.push("llama_cpp");
    }
    // ort gets pushed here when the ort feature lands.
    out
}
