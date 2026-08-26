//! Embedding task — vector representations of text.
//!
//! Wires the global `[embed]` config block to concrete embedder
//! implementations. The trait surface ([`Embedder`], [`EmbedRequest`],
//! [`EmbedResponse`], [`EmbedError`], [`EmbedUsage`]) is defined in
//! the [`claw_embed`] crate so other workspace crates (e.g.
//! `claw-semantic`) can implement / consume embeddings without
//! pulling in the entire `core` runtime.
//!
//! What lives here:
//!
//! - The OpenAI-compatible cloud embedder ([`OpenAICompatEmbedder`])
//!   — needs `crate::config::EmbedConfig` and the agent's credential
//!   resolver, so it can't be pure.
//! - The `build_default` / `build_from` / `build_from_with_agent`
//!   factory tree that reads `crate::config` and constructs whichever
//!   embedder the user has configured.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::config::EmbedConfig;

// Re-export the trait surface so existing call-sites
// (`crate::model::tasks::embed::{Embedder, ...}`) keep compiling
// transparently after the move to claw-embed.
pub use claw_embed::{EmbedError, EmbedRequest, EmbedResponse, EmbedUsage, Embedder};

/// Default embedding model name. Used when [`crate::config::EmbedConfig::model`]
/// is left empty; an explicit `model = "..."` in `[embed]` overrides this.
///
/// **Switching models invalidates every existing row in `semantic.db`.**
/// Vector spaces are model-specific — cosine similarity between vectors
/// from two different models is meaningless, and dimensionality usually
/// differs (`text-embedding-3-small` is 1536, `-large` is 3072).
/// `SemanticStore` enforces this with a stickiness check that returns
/// `ModelMismatch` on the first row from a new model. To migrate, run
/// `cos agent semantic clear-all --yes` and re-index.
pub const MODEL_NAME: &str = "text-embedding-3-small";

// =====================================================================
// Factory
// =====================================================================

/// Build the configured embedder, if any. Returns `Ok(None)` when
/// embedding is disabled (`provider="none"`), or when `provider="auto"`
/// and neither the bundled local Qwen3 stack nor an embeddings-capable
/// cloud `[agent]` provider is available. Returns an error if the
/// config block names a provider that does not exist.
pub fn build_default() -> Result<Option<Box<dyn Embedder>>, String> {
    let cfg = &crate::config::get().embed;
    build_from(cfg)
}

pub fn build_from(cfg: &EmbedConfig) -> Result<Option<Box<dyn Embedder>>, String> {
    build_from_with_agent(cfg, &crate::config::get().agent)
}

/// Variant of [`build_from`] that takes the `[agent]` config
/// explicitly. Lets tests exercise the `provider="agent-auto"` path
/// without depending on global config state.
pub fn build_from_with_agent(
    cfg: &EmbedConfig,
    agent: &crate::config::AgentConfig,
) -> Result<Option<Box<dyn Embedder>>, String> {
    match cfg.provider.as_str() {
        "none" => Ok(None),
        // Default path: prefer the bundled local Qwen3 embedding stack
        // when the image includes both the model and the ort-genai
        // runtime. Linux arm64 builds currently skip that stack because
        // upstream does not publish a Linux arm64 CPU runtime; on those
        // systems we fall back to the user's configured cloud `[agent]`
        // provider, and only return None (semantic recall off) when no
        // embeddings-capable provider is configured at all.
        "auto" | "" => system_local_default(cfg, agent),
        // Compatibility escape hatch for users who deliberately want the
        // old "derive embeddings from my chat provider" behaviour.
        "agent-auto" => derive_from_agent(cfg, agent),
        // Every OpenAI-API-shape backend (OpenAI / Azure / Ollama / vLLM /
        // TGI / LMStudio) shares one impl — switch behaviour via base_url.
        // `azure` differs only in the auth header style (`api-key:` instead
        // of `Authorization: Bearer …`).
        "openai" | "azure" | "xai" | "deepseek" | "openrouter" | "ollama" => {
            Ok(Some(Box::new(OpenAICompatEmbedder::from_config(cfg))))
        }
        // Local Qwen3-Embedding-0.6B via onnxruntime-genai. Reads the
        // model directory from `cfg.model_dir` (or the default registry
        // slot). The model is loaded lazily on first call.
        "qwen3-local" | "local" => Ok(Some(Box::new(super::qwen3_genai::build_from_config(cfg)))),
        other => Err(format!("unknown embed provider: {other}")),
    }
}

/// Default (`provider = "auto"`) embedder resolution.
///
/// 1. Prefer the bundled local Qwen3 ONNX stack when the image ships
///    both the model and the ort-genai runtime.
/// 2. If that stack is unavailable (e.g. Linux arm64, or the model
///    isn't installed), fall back to the user's configured cloud
///    `[agent]` provider when it speaks the OpenAI `/embeddings` shape
///    (openai / azure / xai / deepseek / openrouter / ollama).
/// 3. Otherwise return `None`: semantic recall stays off until the
///    user sets `[embed].provider` explicitly. We never silently
///    substitute a low-quality approximation — an embedding system
///    must produce real, model-consistent vectors or none at all
///    (mixing vector spaces would corrupt `semantic.db`).
fn system_local_default(
    cfg: &EmbedConfig,
    agent: &crate::config::AgentConfig,
) -> Result<Option<Box<dyn Embedder>>, String> {
    let mut local_cfg = cfg.clone();
    local_cfg.provider = "local".to_string();
    let embedder = super::qwen3_genai::build_from_config(&local_cfg);
    if embedder.is_configured() {
        return Ok(Some(Box::new(embedder)));
    }
    // Local stack missing → derive from the user's cloud agent provider.
    match derive_from_agent(cfg, agent) {
        Ok(Some(e)) => {
            tracing::info!(
                "embed: bundled Qwen3 stack unavailable — deriving embeddings from the configured agent provider (set [embed].provider to choose explicitly)"
            );
            Ok(Some(e))
        }
        // Agent provider isn't embeddings-capable, or deriving needs
        // more config (e.g. Azure without an explicit [embed].model).
        // In the default `auto` path we stay best-effort and leave
        // semantic recall off rather than failing the whole runtime.
        Ok(None) => {
            tracing::debug!(
                "embed: no local Qwen3 stack and the agent provider is not embeddings-capable — semantic recall disabled until [embed].provider is set"
            );
            Ok(None)
        }
        Err(e) => {
            tracing::debug!(
                "embed: no local Qwen3 stack and agent-derived embeddings unavailable ({e}) — semantic recall disabled until [embed].provider is set"
            );
            Ok(None)
        }
    }
}

/// Build an embedder by inheriting credentials + base_url from the
/// main `[agent]` block. Used by explicit `provider="agent-auto"`.
/// Returns `Ok(None)` when the main provider
/// doesn't speak the OpenAI `/embeddings` shape (mock / anthropic /
/// gemini / bedrock / etc.). Explicit fields on `cfg` (model,
/// api_key_credential, api_key_env, base_url) win over the derived
/// values — so users can keep `provider="agent-auto"` while pointing the
/// embedder at a separate Azure deployment via `[embed].model`.
fn derive_from_agent(
    cfg: &EmbedConfig,
    agent: &crate::config::AgentConfig,
) -> Result<Option<Box<dyn Embedder>>, String> {
    let alias = match agent.provider.as_str() {
        "openai" | "azure" | "xai" | "deepseek" | "openrouter" | "ollama" => &agent.provider,
        _ => {
            tracing::debug!(
                "embed: [embed].provider=agent-auto and main agent provider={} is not OpenAI-shape — auto-indexing skipped (set [embed].provider explicitly to enable)",
                agent.provider
            );
            return Ok(None);
        }
    };

    let mut derived = cfg.clone();
    derived.provider = alias.to_string();
    if derived
        .base_url
        .as_deref()
        .map(str::is_empty)
        .unwrap_or(true)
    {
        derived.base_url = agent.base_url.clone();
    }
    if derived.api_key_credential.is_none() {
        derived.api_key_credential = agent.api_key_credential.clone();
    }
    if derived.api_key_env.is_none() {
        derived.api_key_env = agent.api_key_env.clone();
    }
    if derived.extra_headers.is_empty() {
        derived.extra_headers = agent.extra_headers.clone();
    }
    // The embed model must be explicitly chosen for non-OpenAI
    // providers — falling back to `MODEL_NAME` silently picks
    // `text-embedding-3-small`, which only exists on plain OpenAI.
    // For Azure / OpenRouter / xAI / DeepSeek / Ollama the same
    // name routes to nothing and the user sees a confusing 404 from
    // the provider instead of a clear configuration error here. Plain
    // OpenAI keeps the MODEL_NAME default because the name is canonical
    // there.
    if derived.model.trim().is_empty() {
        if alias == "openai" {
            derived.model = MODEL_NAME.to_string();
        } else {
            return Err(format!(
                "embed: [embed].model is required when [embed].provider=agent-auto \
                 (provider={alias}); set `model = \"<embedding model name>\"` \
                 explicitly under [embed]"
            ));
        }
    }
    Ok(Some(Box::new(OpenAICompatEmbedder::from_config(&derived))))
}

// =====================================================================
// OpenAI-compatible cloud embedder
// =====================================================================

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_OLLAMA_BASE: &str = "http://localhost:11434/v1";

fn default_base_url_for(alias: &str) -> &'static str {
    match alias {
        "ollama" => DEFAULT_OLLAMA_BASE,
        // xAI / DeepSeek / OpenRouter generally don't run their own embed
        // models — fall through to a sane OpenAI default that the user
        // will almost always override via base_url.
        _ => DEFAULT_OPENAI_BASE,
    }
}

fn alias_is_local_default(alias: &str) -> bool {
    matches!(alias, "ollama")
}

pub struct OpenAICompatEmbedder {
    alias: String,
    base_url: String,
    api_key: Option<String>,
    model: String,
    extra_headers: HashMap<String, String>,
    client: reqwest::Client,
}

impl OpenAICompatEmbedder {
    pub fn from_config(cfg: &EmbedConfig) -> Self {
        let alias = cfg.provider.clone();
        let base_url = cfg
            .base_url
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default_base_url_for(&alias).to_string());
        let base_url = base_url.trim_end_matches('/').to_string();

        let api_key = crate::agent::llm::providers::openai_compat::resolve_api_key(
            cfg.api_key_credential.as_deref(),
            cfg.api_key_env.as_deref(),
        )
        .ok()
        .flatten();

        let timeout = if cfg.request_timeout == 0 {
            Duration::from_secs(0)
        } else {
            Duration::from_secs(cfg.request_timeout)
        };
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            alias,
            base_url,
            api_key,
            // Honor cfg.model when set; fall back to MODEL_NAME if empty.
            // Switching this value invalidates the existing semantic.db —
            // see MODEL_NAME doc for the migration story (`semantic clear-all`).
            model: if cfg.model.trim().is_empty() {
                MODEL_NAME.to_string()
            } else {
                cfg.model.clone()
            },
            extra_headers: cfg.extra_headers.clone(),
            client,
        }
    }

    fn endpoint(&self) -> String {
        // Preserve any query string the user attached to base_url
        // (Azure OpenAI requires `?api-version=...`).
        let (base, query) = match self.base_url.split_once('?') {
            Some((b, q)) => (b.trim_end_matches('/'), Some(q)),
            None => (self.base_url.trim_end_matches('/'), None),
        };
        // Azure has two valid base_url shapes:
        //   (1) deployment URL — `https://acme.openai.azure.com/openai/deployments/<dep>`
        //       (legacy / explicit; user pasted the full deployment endpoint).
        //   (2) resource root — `https://acme.openai.azure.com/`
        //       (matches the chat provider's stored shape; the deployment
        //       name lives in `self.model`).
        // When the base lacks `/openai/deployments/`, we assemble it
        // from `self.model` so the same on-disk URL works for chat
        // AND embed without duplicating the deployment path.
        if self.alias == "azure" && !base.contains("/openai/deployments/") {
            let deployment = self.model.as_str();
            return match query {
                Some(q) => {
                    format!("{base}/openai/deployments/{deployment}/embeddings?{q}")
                }
                None => format!("{base}/openai/deployments/{deployment}/embeddings"),
            };
        }
        match query {
            Some(q) => format!("{base}/embeddings?{q}"),
            None => format!("{base}/embeddings"),
        }
    }
}

#[async_trait]
impl Embedder for OpenAICompatEmbedder {
    fn name(&self) -> &str {
        &self.alias
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn is_configured(&self) -> bool {
        self.api_key.is_some() || alias_is_local_default(&self.alias)
    }

    async fn embed(&self, request: EmbedRequest) -> Result<EmbedResponse, EmbedError> {
        if request.inputs.is_empty() {
            return Err(EmbedError::InvalidInput("inputs must not be empty".into()));
        }
        // OpenAI embeddings accept either a string or an array of strings;
        // pass the array shape always — works for all backends.
        let body = serde_json::json!({
            "model": self.model,
            "input": request.inputs,
        });

        let mut http = self
            .client
            .post(self.endpoint())
            .header("Content-Type", "application/json")
            .json(&body);
        if let Some(key) = &self.api_key {
            // Azure OpenAI authenticates with `api-key: <key>` rather than
            // `Authorization: Bearer …`. Other OpenAI-shape providers all
            // accept the bearer form.
            if self.alias == "azure" {
                http = http.header("api-key", key.as_str());
            } else {
                http = http.bearer_auth(key);
            }
        }
        for (k, v) in &self.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }
        let resp = http
            .send()
            .await
            .map_err(|e| EmbedError::Transport(e.to_string()))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| EmbedError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(classify_http_error(status.as_u16(), &bytes));
        }
        let parsed: WireEmbedResponse =
            serde_json::from_slice(&bytes).map_err(|e| EmbedError::Parse(e.to_string()))?;
        // OpenAI returns data sorted by the index field — but be defensive
        // and re-sort to keep the order matching the request.
        let mut data = parsed.data;
        data.sort_by_key(|r| r.index);
        let embeddings: Vec<Vec<f32>> = data.into_iter().map(|r| r.embedding).collect();
        if embeddings.len() != request.inputs.len() {
            return Err(EmbedError::Parse(format!(
                "expected {} embeddings, got {}",
                request.inputs.len(),
                embeddings.len()
            )));
        }
        let dim = embeddings.first().map(|v| v.len()).unwrap_or(0);
        Ok(EmbedResponse {
            embeddings,
            model: parsed.model.unwrap_or_else(|| self.model.clone()),
            dim,
            usage: EmbedUsage {
                prompt_tokens: parsed.usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                total_tokens: parsed.usage.as_ref().map(|u| u.total_tokens).unwrap_or(0),
            },
        })
    }
}

#[derive(Debug, Deserialize)]
struct WireEmbedResponse {
    data: Vec<WireEmbedDatum>,
    model: Option<String>,
    usage: Option<WireUsage>,
}

#[derive(Debug, Deserialize)]
struct WireEmbedDatum {
    embedding: Vec<f32>,
    #[serde(default)]
    index: u32,
}

#[derive(Debug, Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

fn classify_http_error(status: u16, bytes: &[u8]) -> EmbedError {
    let message = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).chars().take(400).collect());
    match status {
        401 | 403 => EmbedError::Auth,
        429 => EmbedError::RateLimited {
            retry_after_ms: 1000,
        },
        _ => EmbedError::Provider { status, message },
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/model/tasks/embed.rs"
    ));
}
