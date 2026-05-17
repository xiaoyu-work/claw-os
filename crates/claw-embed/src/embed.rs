//! [`Embedder`] trait and request/response types.
//!
//! This module defines the **pure contract** every embedding backend
//! implements. Concrete implementations (cloud HTTP, local
//! `onnxruntime-genai`, mocks) live elsewhere — typically in `core`
//! where they can read global config and talk to the engine package
//! manager.
//!
//! See also: [`crate::store::SemanticStore`], the storage layer that
//! consumes an `Embedder` and persists `(text, vector)` pairs.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One embedding request — a batch of inputs to embed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedRequest {
    pub inputs: Vec<String>,
}

/// Result of an embedding call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedResponse {
    pub embeddings: Vec<Vec<f32>>,
    pub model: String,
    pub dim: usize,
    pub usage: EmbedUsage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmbedUsage {
    pub prompt_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("not configured: set [embed] block in config.json")]
    NotConfigured,
    #[error("authentication failed: bad or missing API key")]
    Auth,
    #[error("rate limited (retry after {retry_after_ms}ms)")]
    RateLimited { retry_after_ms: u64 },
    #[error("provider returned error: {status} — {message}")]
    Provider { status: u16, message: String },
    #[error("transport: {0}")]
    Transport(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

#[async_trait]
pub trait Embedder: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    fn is_configured(&self) -> bool;
    async fn embed(&self, request: EmbedRequest) -> Result<EmbedResponse, EmbedError>;
}
