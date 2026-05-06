//! Inference engines hosted in the model-runtime daemon.
//!
//!   - `ort`   — ONNX Runtime singleton (STT/TTS/Embedding/Vision/ImageGen)
//!   - `llama_cpp` — llama.cpp singleton (LLM, GGUF). Loaded at runtime
//!                   via libloading from the engine package manager
//!                   install — see `core/src/engine_pkg/`.
//!
//! Phase 0.5 defines the trait and ships the llama_cpp scaffolding
//! (libloading-based FFI + safe wrapper). Concrete inference is wired
//! when the user supplies the first GGUF file.

use async_trait::async_trait;

pub mod llama_cpp;

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // Variants surface as the engine layer matures.
pub enum EngineError {
    /// No engine of this name was compile-time linked. Retained for
    /// callers that pre-date P2.3; new code should prefer
    /// [`Self::NotInstalled`].
    #[error("engine not linked: {0}")]
    NotLinked(&'static str),
    /// The engine's runtime files aren't on disk. Hint: install via
    /// `cos engine update <name>`.
    #[error("engine not installed: {0}")]
    NotInstalled(String),
    /// The runtime was found but the OS loader rejected it. Likely a
    /// corrupt download, an ABI mismatch, or a missing dependent
    /// library (sister DLL, MSVC runtime, CUDA runtime, ...).
    #[error("library load failed: {0}")]
    LibraryLoadFailed(String),
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

/// Names of every engine whose runtime is currently installed and
/// loadable. Replaces the old compile-time `IS_LINKED` check — engine
/// availability is now a property of the host's `<engines_dir>` state,
/// not the cos build. `cos agent status` and the agent provider
/// registry use this to decide whether local inference is available.
pub fn engines_linked() -> Vec<&'static str> {
    let mut out = Vec::new();
    if llama_cpp::is_installed() {
        out.push("llama_cpp");
    }
    // ort gets pushed here when the ort engine lands.
    out
}
