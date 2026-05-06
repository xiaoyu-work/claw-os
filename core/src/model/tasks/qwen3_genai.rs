//! Local Qwen3-Embedding-0.6B inference via onnxruntime-genai.
//!
//! ## Architecture
//!
//! Qwen3-Embedding is a Qwen3ForCausalLM checkpoint where the
//! "embedding" is taken to be the **last-token hidden state** of the
//! decoder. Olive exports it as an ONNX-runtime-genai bundle whose
//! graph exposes both the standard `logits` output and an additional
//! named `hidden_states` output of shape `[batch, seq, hidden_size]`.
//!
//! For embedding inference we need exactly one forward pass with no
//! generation, so the call sequence is:
//!
//! 1. Tokenize the input → `OgaSequences` (one sequence of i32 ids).
//! 2. `OgaGenerator_AppendTokens` — runs the model forward exactly
//!    once and materializes both named outputs in the generator.
//! 3. `OgaGenerator_GetOutput("hidden_states")` → `OgaTensor` of
//!    shape `[1, L, hidden_size]`.
//! 4. Slice the last token's row, L2-normalize, return.
//!
//! We do **not** call `OgaGenerator_GenerateNextToken` because it
//! would consume an extra forward pass without changing the output
//! we read. The `requires_engine` pin in the model manifest keeps us
//! locked to the engine version this calling convention was verified
//! against.
//!
//! ## Thread safety
//!
//! The embedder caches the loaded `OgaModel` and `OgaTokenizer` across
//! calls (the model is ~600MB; reloading per call is unacceptable),
//! but the upstream C API is **not thread-safe**. To keep the
//! [`Embedder`] trait's `Send + Sync` contract we serialize every
//! call through a `tokio::sync::Mutex`. Embedding is a few-ms
//! operation so contention is acceptable.

use std::path::PathBuf;

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::embed::{EmbedError, EmbedRequest, EmbedResponse, EmbedUsage, Embedder};
use crate::model::engines::ort_genai::runtime::OrtGenaiRuntime;
use crate::model::engines::ort_genai::safe::{
    OgaGenerator, OgaGeneratorParams, OgaModel, OgaTokenizer, OrtGenaiError,
};

/// Canonical model name reported in [`EmbedResponse::model`]. The
/// SemanticStore stickiness guard pins a corpus to whatever value it
/// sees on the first index call, so this name is ALSO the corpus key.
pub const MODEL_NAME: &str = "Qwen3-Embedding-0.6B";

/// Qwen3-Embedding-0.6B canonical hidden size. Verified by Olive
/// export: `genai_config.json` declares `hidden_size = 1024`.
/// The embedder asserts the runtime tensor matches this.
pub const HIDDEN_SIZE: usize = 1024;

/// Maximum number of input tokens we'll accept per call. The model
/// context is 32K, but for embedding the usual operating point is
/// well under 8K — a longer cap risks pathological allocation when
/// users accidentally feed in a giant document. Truncate at the
/// caller for documents above this.
const DEFAULT_MAX_TOKENS: usize = 8192;

/// Convert from the safe wrapper's error to the embedder error.
impl From<OrtGenaiError> for EmbedError {
    fn from(e: OrtGenaiError) -> Self {
        match e {
            OrtGenaiError::EmptyTokens => {
                EmbedError::InvalidInput("input tokenized to zero tokens".into())
            }
            OrtGenaiError::InputWithNul | OrtGenaiError::PathWithNul(_) => {
                EmbedError::InvalidInput(format!("input contains NUL byte: {e}"))
            }
            other => EmbedError::Provider {
                status: 500,
                message: other.to_string(),
            },
        }
    }
}

struct Inner {
    // Field declaration order = drop order. Tokenizer drops first
    // (its handle was created from the model and *may* internally
    // reference model state). Then model drops. Per-call params /
    // generator / tensor are locals in `embed_one` and drop in the
    // reverse order they were declared.
    tokenizer: OgaTokenizer,
    model: OgaModel,
}

/// Local Qwen3-Embedding-0.6B embedder backed by onnxruntime-genai.
pub struct Qwen3GenaiEmbedder {
    /// Lazily-loaded model + tokenizer state. `None` means the
    /// embedder is configured but the model hasn't been loaded yet —
    /// we defer load until the first call so a misconfigured engine
    /// path doesn't bring the whole agent runtime down at startup.
    inner: Mutex<Option<Inner>>,
    /// Path to the Olive-exported model directory.
    model_dir: PathBuf,
    /// Reported in [`EmbedResponse::model`] — controls SemanticStore
    /// stickiness keying.
    model_name: String,
    max_input_tokens: usize,
}

impl Qwen3GenaiEmbedder {
    /// Build an embedder pointing at an Olive-exported model directory.
    /// The model is **not** loaded here — load happens on the first
    /// `embed()` call.
    pub fn new(model_dir: impl Into<PathBuf>) -> Self {
        Self {
            inner: Mutex::new(None),
            model_dir: model_dir.into(),
            model_name: MODEL_NAME.to_string(),
            max_input_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    /// Default model directory under the cos data root:
    /// `<models_dir>/qwen3-embedding-0.6b/v1/`.
    pub fn default_model_dir() -> PathBuf {
        crate::model::paths::model_version_dir("qwen3-embedding-0.6b", "v1")
    }

    fn ensure_model_dir(&self) -> Result<(), EmbedError> {
        if !self.model_dir.exists() {
            return Err(EmbedError::NotConfigured);
        }
        if !self.model_dir.is_dir() {
            return Err(EmbedError::InvalidInput(format!(
                "model dir is not a directory: {}",
                self.model_dir.display()
            )));
        }
        if !self.model_dir.join("genai_config.json").exists() {
            return Err(EmbedError::InvalidInput(format!(
                "model dir is missing genai_config.json: {}",
                self.model_dir.display()
            )));
        }
        Ok(())
    }

    fn load_inner(&self) -> Result<Inner, EmbedError> {
        self.ensure_model_dir()?;
        let runtime = OrtGenaiRuntime::shared().map_err(|e| EmbedError::Provider {
            status: 500,
            message: format!("ort-genai engine: {e}"),
        })?;
        let model = OgaModel::load(runtime, &self.model_dir)?;
        let tokenizer = OgaTokenizer::new(&model)?;
        Ok(Inner { tokenizer, model })
    }

    fn embed_one(inner: &Inner, text: &str, max_input_tokens: usize) -> Result<Vec<f32>, EmbedError> {
        if text.is_empty() {
            return Err(EmbedError::InvalidInput("empty input".into()));
        }
        let seqs = inner.tokenizer.encode(text)?;
        let ids: Vec<i32> = seqs.first_sequence().to_vec();
        if ids.is_empty() {
            return Err(EmbedError::InvalidInput(
                "tokenizer produced zero tokens".into(),
            ));
        }
        let token_count = ids.len();
        if token_count > max_input_tokens {
            return Err(EmbedError::InvalidInput(format!(
                "input too long: {token_count} tokens > max {max_input_tokens}; truncate at the caller"
            )));
        }
        let mut params = OgaGeneratorParams::new(&inner.model)?;
        params.set_search_number("max_length", (token_count + 1) as f64)?;
        params.set_search_number("batch_size", 1.0)?;
        let mut gen = OgaGenerator::new(&inner.model, &params)?;
        gen.append_tokens(&ids)?;
        let tensor = gen.get_output("hidden_states")?;
        let shape = tensor.shape()?;
        if shape.len() != 3 {
            return Err(EmbedError::Provider {
                status: 500,
                message: format!("hidden_states shape rank {} != 3 (got {shape:?})", shape.len()),
            });
        }
        let batch = shape[0] as usize;
        let seq_len = shape[1] as usize;
        let hidden = shape[2] as usize;
        if batch != 1 {
            return Err(EmbedError::Provider {
                status: 500,
                message: format!("hidden_states batch != 1: shape {shape:?}"),
            });
        }
        if hidden != HIDDEN_SIZE {
            return Err(EmbedError::Provider {
                status: 500,
                message: format!(
                    "hidden_states dim {hidden} != expected {HIDDEN_SIZE} (shape {shape:?})"
                ),
            });
        }
        if seq_len != token_count {
            // Defensive — should never happen because AppendTokens runs
            // a forward pass on exactly the appended tokens.
            return Err(EmbedError::Provider {
                status: 500,
                message: format!(
                    "hidden_states seq_len {seq_len} != token_count {token_count} (shape {shape:?})"
                ),
            });
        }
        let data = tensor.data_f32()?;
        let row_start = (seq_len - 1) * hidden;
        let row_end = row_start + hidden;
        if row_end > data.len() {
            return Err(EmbedError::Provider {
                status: 500,
                message: format!(
                    "hidden_states data short: have {} f32, want {}",
                    data.len(),
                    row_end
                ),
            });
        }
        let mut v = data[row_start..row_end].to_vec();
        l2_normalize(&mut v);
        Ok(v)
    }
}

fn l2_normalize(v: &mut [f32]) {
    let mut sumsq = 0.0_f64;
    for x in v.iter() {
        sumsq += (*x as f64) * (*x as f64);
    }
    let norm = sumsq.sqrt() as f32;
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[async_trait]
impl Embedder for Qwen3GenaiEmbedder {
    fn name(&self) -> &str {
        "qwen3-local"
    }

    fn model(&self) -> &str {
        &self.model_name
    }

    fn is_configured(&self) -> bool {
        self.model_dir.exists() && self.model_dir.join("genai_config.json").exists()
    }

    async fn embed(&self, request: EmbedRequest) -> Result<EmbedResponse, EmbedError> {
        if request.inputs.is_empty() {
            return Err(EmbedError::InvalidInput(
                "embed request has no inputs".into(),
            ));
        }
        // Bring up the model on first use.
        let mut guard = self.inner.lock().await;
        if guard.is_none() {
            *guard = Some(self.load_inner()?);
        }
        // SAFETY for clippy: guard is locked, we have exclusive access.
        let inner = guard.as_ref().expect("just loaded");

        let mut embeddings = Vec::with_capacity(request.inputs.len());
        let mut total_tokens: u32 = 0;
        for text in &request.inputs {
            let v = Self::embed_one(inner, text, self.max_input_tokens)?;
            // Tokenize again only if accurate accounting is needed.
            // For now we count post-fact via the embed size, which is
            // O(L) per input so we just record the input string char
            // count as an approximation — usage is informational only
            // for local inference (no per-token billing).
            total_tokens = total_tokens.saturating_add(text.chars().count() as u32);
            embeddings.push(v);
        }
        let dim = embeddings.first().map(|v| v.len()).unwrap_or(0);
        Ok(EmbedResponse {
            embeddings,
            model: self.model_name.clone(),
            dim,
            usage: EmbedUsage {
                prompt_tokens: total_tokens,
                total_tokens,
            },
        })
    }
}

/// Resolve the model dir for the local Qwen3 embedder:
///   1. Explicit override in [`crate::config::EmbedConfig::model_dir`].
///   2. Default model registry slot
///      `<models_dir>/qwen3-embedding-0.6b/v1/`.
pub fn resolve_model_dir(cfg: &crate::config::EmbedConfig) -> PathBuf {
    if let Some(custom) = cfg.model_dir.as_ref() {
        if !custom.is_empty() {
            return PathBuf::from(custom);
        }
    }
    Qwen3GenaiEmbedder::default_model_dir()
}

/// Build a [`Qwen3GenaiEmbedder`] from an [`EmbedConfig`]. Does not
/// load the model — callers should `await embed.embed(...)` to bring
/// the engine up.
pub fn build_from_config(cfg: &crate::config::EmbedConfig) -> Qwen3GenaiEmbedder {
    Qwen3GenaiEmbedder::new(resolve_model_dir(cfg))
}

/// Sanity-check the runtime / model dir without actually loading the
/// model. Used by `cos agent semantic status` to give a useful error
/// when the local embedder is configured but the engine isn't.
pub fn precheck(cfg: &crate::config::EmbedConfig) -> Result<(), String> {
    let dir = resolve_model_dir(cfg);
    if !dir.exists() {
        return Err(format!(
            "model dir does not exist: {} (run `cos model import <olive-export-dir> --as qwen3-embedding-0.6b`)",
            dir.display()
        ));
    }
    if !dir.join("genai_config.json").exists() {
        return Err(format!(
            "{} missing genai_config.json — is this a valid onnxruntime-genai export?",
            dir.display()
        ));
    }
    if !crate::model::engines::ort_genai::is_installed() {
        return Err(
            "ort-genai engine is not installed; run `cos engine install ort-genai --from <release.zip>`"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_unit_vector_is_idempotent() {
        let mut v = vec![3.0f32, 4.0, 0.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm = {norm}");
    }

    #[test]
    fn l2_normalize_zero_vector_remains_zero() {
        let mut v = vec![0.0f32; 4];
        l2_normalize(&mut v);
        assert!(v.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn precheck_reports_missing_dir() {
        let mut cfg = crate::config::EmbedConfig::default();
        cfg.provider = "qwen3-local".into();
        cfg.model_dir = Some("/definitely/does/not/exist/qwen3".to_string());
        let err = precheck(&cfg).unwrap_err();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn embedder_constructs_with_explicit_dir() {
        let e = Qwen3GenaiEmbedder::new("/some/path");
        assert_eq!(e.name(), "qwen3-local");
        assert_eq!(e.model(), MODEL_NAME);
        assert!(!e.is_configured(), "non-existent path should not be configured");
    }

    #[test]
    fn resolve_model_dir_falls_back_to_default() {
        let mut cfg = crate::config::EmbedConfig::default();
        cfg.model_dir = None;
        let dir = resolve_model_dir(&cfg);
        // Pinned default — if the registry layout changes, this test
        // catches it.
        assert!(dir.ends_with("qwen3-embedding-0.6b/v1") || dir.ends_with("qwen3-embedding-0.6b\\v1"));
    }

    #[test]
    fn resolve_model_dir_uses_explicit_override() {
        let mut cfg = crate::config::EmbedConfig::default();
        cfg.model_dir = Some("C:\\custom\\qwen".to_string());
        assert_eq!(resolve_model_dir(&cfg), PathBuf::from("C:\\custom\\qwen"));
    }

    #[test]
    fn empty_inputs_rejected() {
        let e = Qwen3GenaiEmbedder::new("/nonexistent");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let res = rt.block_on(e.embed(EmbedRequest { inputs: vec![] }));
        match res {
            Err(EmbedError::InvalidInput(_)) => {}
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }
}
