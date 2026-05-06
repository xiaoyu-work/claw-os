//! LLM abstraction layer.
//!
//! `Provider` is the stable contract that every model backend implements.
//! Phase 0 ships the trait + types + a default registry. Phase 1 implements
//! `providers::anthropic`. Phase 4 implements the rest of the top-9 (Q3):
//! openai, gemini, openrouter, ollama, bedrock, custom, xai, deepseek.
//! `providers::local` (Phase 0.5+) routes to the in-process llama.cpp engine
//! exposed via crate::model::tasks::llm.

pub mod providers;
pub mod metadata;
pub mod rate_limit;
pub mod registry;
pub mod run_log;
pub mod types;
pub mod usage;

pub use types::{
    ChatRequest, ChatResponse, ContentBlock, EngineInfo, FinishReason, Message, Role,
    StreamEvent, Tool, ToolCall, ToolChoice, Usage,
};

use async_trait::async_trait;
use futures_util::stream::BoxStream;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("provider not configured: {0}")]
    NotConfigured(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("provider transport error: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("provider returned error: {status} — {message}")]
    Provider { status: u16, message: String },

    #[error("rate limited; retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },

    #[error("authentication failed")]
    Auth,

    #[error("response could not be parsed: {0}")]
    Parse(String),

    #[error("stream error: {0}")]
    Stream(String),

    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, LlmError>;

/// Stable contract every LLM backend implements.
///
/// Implementations should be cheap to clone (intended to be wrapped in `Arc`
/// at the registry level). Network state and rate limiters live behind the
/// implementation.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Stable identifier (e.g., "anthropic", "openai", "ollama", "local").
    fn name(&self) -> &str;

    /// Models this provider can serve. May be a static list or fetched lazily.
    fn supported_models(&self) -> Vec<String>;

    /// Whether the provider has the credentials / endpoint it needs to run.
    fn is_configured(&self) -> bool;

    /// Information about the engine actually executing inference.
    /// Default: `None` (cloud providers — the engine is the upstream
    /// API, not under our audit purview). Local providers should
    /// return `Some(...)` derived from the **loaded** runtime once
    /// it's up. Returning `None` before the engine is loaded is fine
    /// — the run-record consumer logs `null` for those fields.
    ///
    /// MUST be cheap (sync, lock-free or near-lock-free). Called from
    /// the per-turn audit path.
    fn engine_info(&self) -> Option<EngineInfo> {
        None
    }

    /// Buffered (non-streaming) chat completion.
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;

    /// Streaming chat completion. Implementations that lack native streaming
    /// may emit a single `StreamEvent::Message` followed by `StreamEvent::Done`.
    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>>;
}

/// Names of providers currently linked into the binary.
pub fn available_providers() -> Vec<&'static str> {
    registry::REGISTERED.to_vec()
}
