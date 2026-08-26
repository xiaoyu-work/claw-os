//! Embedding interface.
//!
//! Phase 1 only ships [`StubEmbedder`] — a deterministic-from-hash
//! 384-dim vector generator. It's enough to wire the rest of the
//! plumbing (chunk → vec → store → search) without pulling in a
//! 100 MB ONNX runtime + model weights.
//!
//! Phase 2 will add a `FastEmbedEmbedder` here using the `fastembed`
//! crate (BGE-small-en-v1.5, ~30 MB, 384-dim, all in-process — no
//! ollama daemon required). The trait signature is intentionally
//! batch-shaped because all embedding models are throughput-bound.

use anyhow::Result;
use sha2::{Digest, Sha256};

/// Fixed embedding dimension for Phase 1. Picked to match
/// BGE-small-en-v1.5 so the on-disk vector store stays compatible
/// when we swap in the real embedder.
pub const EMBED_DIM: usize = 384;

pub trait Embedder: Send + Sync {
    fn dim(&self) -> usize {
        EMBED_DIM
    }

    /// Embed N strings → N vectors. Implementations are encouraged to
    /// batch internally; the caller does not pre-split.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

/// Deterministic stand-in: SHA-256 of the input expanded to
/// `EMBED_DIM` floats in [-1, 1].
///
/// Search results from this embedder are gibberish, but every other
/// layer is exercised: chunks get a stable fingerprint, the store
/// roundtrips them, and the CLI returns whatever the cosine-similar
/// neighbours happen to be. Use only for plumbing tests, never in a
/// shipped image where the user expects real semantic results.
pub struct StubEmbedder;

impl Embedder for StubEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            let mut v = Vec::with_capacity(EMBED_DIM);
            let mut hasher = Sha256::new();
            hasher.update(t.as_bytes());
            let mut seed = hasher.finalize().to_vec();
            // Expand 32 bytes → EMBED_DIM floats by repeatedly
            // hashing the running seed.
            while v.len() < EMBED_DIM {
                for chunk in seed.chunks(4) {
                    if v.len() >= EMBED_DIM {
                        break;
                    }
                    let bytes: [u8; 4] = chunk.try_into().unwrap_or([0; 4]);
                    let u = u32::from_le_bytes(bytes);
                    let f = (u as f32 / u32::MAX as f32) * 2.0 - 1.0;
                    v.push(f);
                }
                let mut h = Sha256::new();
                h.update(&seed);
                seed = h.finalize().to_vec();
            }
            // L2-normalise so the cosine similarity in the store
            // collapses to a plain dot product.
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            for x in &mut v {
                *x /= norm;
            }
            out.push(v);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/embed.rs"
    ));
}
