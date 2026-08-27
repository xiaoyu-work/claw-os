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
use sha2::{Digest, Sha256};

/// Fixed dimension for the deterministic compatibility embedder.
pub const EMBED_DIM: usize = 384;

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

/// Deterministic stand-in used by the filesystem semantic prototype.
///
/// It preserves the prototype's existing 384-dimensional vectors and
/// `"stub-sha256"` model identity. Search results are deterministic but are
/// not semantically meaningful.
#[derive(Debug, Default)]
pub struct StubEmbedder;

#[async_trait]
impl Embedder for StubEmbedder {
    fn name(&self) -> &str {
        "stub-sha256"
    }

    fn model(&self) -> &str {
        "stub-sha256"
    }

    fn is_configured(&self) -> bool {
        true
    }

    async fn embed(&self, request: EmbedRequest) -> Result<EmbedResponse, EmbedError> {
        let mut embeddings = Vec::with_capacity(request.inputs.len());
        for input in &request.inputs {
            embeddings.push(stub_embedding(input));
        }
        Ok(EmbedResponse {
            embeddings,
            model: self.model().to_string(),
            dim: EMBED_DIM,
            usage: EmbedUsage::default(),
        })
    }
}

fn stub_embedding(text: &str) -> Vec<f32> {
    let mut embedding = Vec::with_capacity(EMBED_DIM);
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let mut seed = hasher.finalize().to_vec();
    while embedding.len() < EMBED_DIM {
        for chunk in seed.chunks(4) {
            if embedding.len() >= EMBED_DIM {
                break;
            }
            let bytes: [u8; 4] = chunk.try_into().unwrap_or([0; 4]);
            let value = u32::from_le_bytes(bytes);
            embedding.push((value as f32 / u32::MAX as f32) * 2.0 - 1.0);
        }
        let mut hasher = Sha256::new();
        hasher.update(&seed);
        seed = hasher.finalize().to_vec();
    }
    let norm = embedding
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt()
        .max(1e-12);
    for value in &mut embedding {
        *value /= norm;
    }
    embedding
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/embed.rs"));
}
