//! LLM abstraction layer.
//!
//! `Provider` is the stable contract that every model backend implements.
//! Phase 0 ships the trait + types + a default registry. Phase 1 implements
//! `providers::anthropic`. Phase 4 implements the rest of the top-9 (Q3):
//! openai, gemini, openrouter, ollama, bedrock, custom, xai, deepseek.
//! `providers::local` (Phase 0.5+) routes to the in-process llama.cpp engine
//! exposed via crate::model::tasks::llm.

pub mod accumulate;
pub mod attempt_observer;
pub mod auxiliary;
pub mod aws_eventstream;
pub mod construction;
pub mod credential_pool;
pub mod error_classifier;
pub mod metadata;
pub mod provider_chain;
pub mod providers;
pub mod rate_limit;
pub mod registry;
pub mod run_log;
pub mod sigv4;
pub mod sse;
pub mod types;
pub mod usage;

pub use provider_chain::{ProviderFallbackState, ProviderSwitch};
pub use types::{
    ChatRequest, ChatResponse, ContentBlock, EngineInfo, FinishReason, Message, Role, StreamEvent,
    Tool, ToolCall, ToolChoice, Usage,
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

    #[error(
        "credential store error for `{credential}` in namespace `agent`: {source}. \
         Repair or replace it with `cos credential revoke {credential} --namespace agent`, \
         then configure the credential again"
    )]
    CredentialStore {
        credential: String,
        #[source]
        source: crate::credential::CredentialError,
    },

    #[error(transparent)]
    Infrastructure(#[from] ProviderInfrastructureError),

    #[error("response could not be parsed: {0}")]
    Parse(String),

    /// Upstream produced a malformed / oversized / non-conforming
    /// response that we can't trust. Distinct from `Parse` (caller's
    /// fault for an unknown shape) — this is *the upstream's* fault
    /// and credential pools should penalise the key that produced it.
    #[error("upstream produced malformed response: {0}")]
    UpstreamMalformed(String),

    #[error("stream error: {0}")]
    Stream(String),

    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, LlmError>;

#[derive(Debug, thiserror::Error)]
pub enum ProviderInfrastructureError {
    #[error("failed to build provider HTTP transport")]
    HttpTransport {
        #[source]
        source: reqwest::Error,
    },

    #[error(transparent)]
    CredentialPool(#[from] credential_pool::PoolError),

    #[error(
        "provider infrastructure state '{component}' is unavailable because its lock was poisoned"
    )]
    StatePoisoned { component: &'static str },
}

impl From<credential_pool::PoolError> for LlmError {
    fn from(error: credential_pool::PoolError) -> Self {
        ProviderInfrastructureError::from(error).into()
    }
}

// ---------------------------------------------------------------------------
// Cross-cutting hardening utilities.
// ---------------------------------------------------------------------------

/// Hard cap on non-streaming response bodies. Any provider that reads
/// `response.bytes()` should funnel through [`read_body_capped`] with
/// this constant so a hostile upstream can't OOM the kernel.
pub const MAX_NONSTREAM_BODY_BYTES: usize = 64 * 1024 * 1024; // 64 MiB

/// Hard cap on streaming response total bytes. Used to bound the
/// cumulative output of a long-running SSE / event-stream so an
/// adversarial upstream can't pump infinitely.
pub const MAX_STREAM_TOTAL_BYTES: usize = 256 * 1024 * 1024; // 256 MiB

/// Drain a `reqwest::Response` body into memory, refusing to allocate
/// more than `max_bytes`. Returns [`LlmError::UpstreamMalformed`] when
/// the upstream attempts to send a body larger than the cap (either
/// via a known `Content-Length` or by streaming past the threshold).
///
/// Lives in the LLM module so every provider can call it without
/// reaching into the kernel; see also `MAX_NONSTREAM_BODY_BYTES`.
pub async fn read_body_capped(resp: reqwest::Response, max_bytes: usize) -> Result<bytes::Bytes> {
    // If the upstream advertised a Content-Length, refuse early to
    // avoid even starting the download.
    if let Some(cl) = resp.content_length() {
        if cl as usize > max_bytes {
            return Err(LlmError::UpstreamMalformed(format!(
                "response Content-Length {cl} exceeds cap {max_bytes}"
            )));
        }
    }
    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(LlmError::Transport)?;
        if buf.len().saturating_add(chunk.len()) > max_bytes {
            return Err(LlmError::UpstreamMalformed(format!(
                "response body exceeded {max_bytes} bytes"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(bytes::Bytes::from(buf))
}

/// Truncate a string to at most `max` chars, never panicking on a
/// non-ASCII byte boundary. Replacement for the `&s[..n]` idiom.
/// Adds a single ellipsis when truncation occurred.
pub fn truncate_for_display(s: &str, max: usize) -> String {
    let take: String = s.chars().take(max).collect();
    if take.chars().count() < s.chars().count() {
        format!("{take}…")
    } else {
        take
    }
}

/// Redact a response body for safe inclusion in log / error fields.
///
/// Provider error bodies routinely contain echoed prompts and partial
/// keys. We trim aggressively (≤ 200 chars), strip control characters,
/// and mask anything that looks like a bearer / API key.
pub fn redact_body_for_error(body: &str) -> String {
    // Mask common bearer / API key patterns BEFORE truncation so a
    // key on the right side isn't preserved by accident.
    let masked = mask_bearer_like(body);
    let truncated = truncate_for_display(&masked, 200);
    truncated
        .chars()
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .collect()
}

fn mask_bearer_like(s: &str) -> String {
    // Match sequences that look like API keys / OAuth tokens and
    // replace the middle with `***`. Heuristic-only; we'd rather
    // over-mask than leak.
    let mut out = String::with_capacity(s.len());
    let chars = s.chars().peekable();
    let mut current = String::new();
    for c in chars {
        // A "token-like" run is alphanumeric + a few separators, ≥ 24 chars.
        let is_token_char = c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.';
        if is_token_char {
            current.push(c);
            continue;
        }
        if current.len() >= 24 {
            out.push_str(&current.chars().take(4).collect::<String>());
            out.push_str("***");
            out.push_str(
                &current
                    .chars()
                    .rev()
                    .take(4)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>(),
            );
        } else {
            out.push_str(&current);
        }
        current.clear();
        out.push(c);
    }
    if current.len() >= 24 {
        out.push_str(&current.chars().take(4).collect::<String>());
        out.push_str("***");
        out.push_str(
            &current
                .chars()
                .rev()
                .take(4)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>(),
        );
    } else {
        out.push_str(&current);
    }
    out
}

#[cfg(test)]
mod cross_cutting_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/cross_cutting_tests.rs"
    ));
}

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

    /// Whether this provider supports prompt caching (today: Anthropic
    /// only; OpenAI's automatic caching is server-side and needs no
    /// markers from us). When `true`, the runtime turn dispatcher
    /// attaches `__cache_system` and `__cache_tools` markers via
    /// [`crate::agent::prompt::caching`] so the provider's
    /// `build_request_body` puts `cache_control: {"type":"ephemeral"}`
    /// on the system prompt and the last tool definition. Default: `false`.
    fn supports_prompt_cache(&self) -> bool {
        false
    }

    fn effective_provider_name(&self) -> String {
        self.name().to_string()
    }

    fn effective_model_name(&self, requested: &str) -> String {
        requested.to_string()
    }

    fn fallback_state(&self) -> Option<ProviderFallbackState> {
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
