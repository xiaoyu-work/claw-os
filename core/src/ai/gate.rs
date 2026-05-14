//! The App–AI Gate. Single entry point apps use to reach a model.
//!
//! ```text
//!     cos agent chat --app …    apps/_lib/ai.py
//!          │                          │
//!          ▼                          ▼
//!   ai::gate::chat_blocking ─── caps::require(ai.*, name(model))
//!          │                          │
//!          │                          ▼
//!          │                  manifest.ai allowlist (models, origins)
//!          │                          │
//!          │                          ▼
//!          │                  budget::reserve (hard-deny overcap)
//!          │                          │
//!          │                          ▼
//!          │                  safety::redact (Strict / Standard)
//!          │                          │
//!          │                          ▼
//!          └──────►  llm::registry::build(provider).chat(ChatRequest)
//!                                       │
//!                                       ▼
//!                              budget::settle (actual usage)
//! ```
//!
//! Strict-mode is on by default for the OS; an app whose manifest has
//! no `ai` block can never reach a model — manifest validation refuses
//! to register such an app in the first place if it declared an
//! `ai.*` need. This file is the runtime defence on top of that:
//! even if a session somehow carries an AI verb, the gate still
//! re-checks the manifest's model + origin allowlist.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use async_trait::async_trait;
use futures_util::stream::BoxStream;

use crate::agent::llm::{
    self,
    types::{ChatRequest as LlmChatRequest, ChatResponse as LlmChatResponse, ContentBlock,
            EngineInfo, Message, Role, StreamEvent},
    Provider as LlmProvider, Result as LlmResult,
};
use crate::agent::safety::redact::Redactor;
use crate::apps;
use crate::caps::{self, Scope, Verb};
use crate::caps::manifest::{AiSafety, PromptOrigin};
use crate::config;

use super::budget::{BudgetError, Store};

// ---------------------------------------------------------------------------
// Public request / response shapes
// ---------------------------------------------------------------------------

/// One-shot chat request handed in by the CLI / `_lib`.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub app_id: String,
    pub verb: String,
    pub model: Option<String>,
    pub origin: String,
    pub prompt: String,
    pub system: Option<String>,
    pub max_units: Option<u64>,
}

/// Structured envelope returned to apps. Always JSON-serialisable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResult {
    pub text: String,
    pub model: String,
    pub provider: String,
    pub usage: Usage,
    pub budget: BudgetReport,
    pub review: ReviewReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub units: u64,
    pub usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetReport {
    pub period: String,
    pub units_used: u64,
    pub units_cap: u64,
    pub usd_used: f64,
    pub usd_cap: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewReport {
    /// Safety profile actually applied to this call.
    pub safety: String,
    /// True if the prompt was modified by the redactor before being
    /// sent upstream. The redacted text itself is not echoed back.
    pub prompt_redacted: bool,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("unknown app `{0}` — not present in COS_APPS_DIR")]
    UnknownApp(String),

    #[error("app `{app}` has no `ai` block in its manifest — AI is disabled for it")]
    NoAiPolicy { app: String },

    #[error("unknown AI verb `{0}` — try ai.chat, ai.chat.untrusted, ai.embed, ai.image.generate, ai.image.analyze, ai.audio.tts, ai.audio.stt, ai.vision.analyze")]
    UnknownVerb(String),

    #[error("invalid prompt origin `{0}` — try trusted, user-input, external-content")]
    BadOrigin(String),

    #[error(
        "origin `{got}` is not in the app's declared origins; \
         allowed: {allowed:?}"
    )]
    OriginNotAllowed {
        got: String,
        allowed: Vec<String>,
    },

    #[error(
        "model `{got}` does not match any of the app's declared model globs: {allowed:?}"
    )]
    ModelNotAllowed {
        got: String,
        allowed: Vec<String>,
    },

    #[error("app `{0}` declared no AI models — cannot resolve a default")]
    NoDefaultModel(String),

    #[error("origin `external-content` requires the `ai.chat.untrusted` verb, not `{verb}`")]
    UntrustedVerbRequired { verb: String },

    #[error("capability denied: {0}")]
    Denied(serde_json::Value),

    #[error("{0}")]
    Budget(#[from] BudgetError),

    #[error("provider error: {0}")]
    Provider(String),

    #[error("safety violation: {0}")]
    Safety(String),

    #[error("internal: {0}")]
    Internal(String),
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Blocking entry point. Wraps the async path in a fresh runtime — the
/// CLI is naturally synchronous and we don't want every app to know
/// about tokio.
pub fn chat_blocking(req: ChatRequest) -> Result<ChatResult, AiError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| AiError::Internal(e.to_string()))?;
    rt.block_on(chat(req))
}

/// Async entry point. Performs the full gate sequence.
pub async fn chat(req: ChatRequest) -> Result<ChatResult, AiError> {
    // 1. Locate the app and its AI policy.
    let app = lookup_app(&req.app_id)?;
    let policy = app
        .manifest
        .ai
        .as_ref()
        .ok_or_else(|| AiError::NoAiPolicy {
            app: req.app_id.clone(),
        })?
        .clone();

    // 2. Parse and validate the requested verb.
    let verb = parse_verb(&req.verb)?;

    // 3. Parse and validate origin against the manifest's allowlist.
    let origin = parse_origin(&req.origin)?;
    if !policy.origins.contains(&origin) {
        return Err(AiError::OriginNotAllowed {
            got: req.origin.clone(),
            allowed: policy
                .origins
                .iter()
                .map(origin_label)
                .collect(),
        });
    }

    // Hardened verb rule: external-content must use `ai.chat.untrusted`.
    if origin == PromptOrigin::ExternalContent && verb == Verb::AI_CHAT {
        return Err(AiError::UntrustedVerbRequired {
            verb: req.verb.clone(),
        });
    }

    // 4. Resolve the model. The app can omit it (then we use the
    //    first declared glob's literal form if any) — otherwise the
    //    requested model must match one of the manifest globs.
    let model = match &req.model {
        Some(m) => {
            if !matches_any_glob(m, &policy.models) {
                return Err(AiError::ModelNotAllowed {
                    got: m.clone(),
                    allowed: policy.models.clone(),
                });
            }
            m.clone()
        }
        None => default_model_from_globs(&policy.models, &req.app_id)?,
    };

    // 5. Capability check at the kernel boundary.
    caps::require(verb, Scope::name(&model))
        .map_err(|d| AiError::Denied(d.to_json()))?;

    // 6. Reserve budget. We have no way to know the exact upstream
    //    cost ahead of time; estimate from the prompt length.
    let estimated_units = estimate_units(&req.prompt, req.max_units);
    let estimated_usd = estimate_usd(estimated_units);
    let mut store = Store::open().map_err(AiError::Internal)?;
    let _reserved = store.reserve(
        &req.app_id,
        estimated_units,
        estimated_usd,
        policy.budget.monthly_units,
        policy.budget.monthly_usd,
    )?;

    // 7. Apply safety pipeline.
    let (prompt_for_provider, prompt_redacted) = apply_safety(&req.prompt, policy.safety);

    // 8. Build the provider request. We use the agent's currently
    //    configured provider; routing per-model is Phase 8 work.
    let cfg = &config::get().agent;
    let provider = llm::registry::build(&cfg.provider, &model, cfg)
        .map_err(|e| AiError::Provider(e.to_string()))?;

    let llm_req = build_chat_request(&model, &prompt_for_provider, req.system.as_deref());
    let llm_resp = provider
        .chat(llm_req)
        .await
        .map_err(|e| AiError::Provider(e.to_string()))?;

    // 9. Extract the text body.
    let text = llm_resp
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    // 10. Settle the budget against actuals.
    let actual_units =
        llm_resp.usage.input_tokens as i64 + llm_resp.usage.output_tokens as i64;
    let delta_units = actual_units - estimated_units as i64;
    let delta_usd = estimate_usd(actual_units.max(0) as u64) - estimated_usd;
    let snapshot = store
        .settle(&req.app_id, delta_units, delta_usd)
        .map_err(AiError::Budget)?;

    Ok(ChatResult {
        text,
        model,
        provider: provider.name().to_string(),
        usage: Usage {
            input_tokens: llm_resp.usage.input_tokens,
            output_tokens: llm_resp.usage.output_tokens,
            units: actual_units.max(0) as u64,
            usd: estimate_usd(actual_units.max(0) as u64),
        },
        budget: BudgetReport {
            period: snapshot.period,
            units_used: snapshot.units_used,
            units_cap: policy.budget.monthly_units,
            usd_used: snapshot.usd_used,
            usd_cap: policy.budget.monthly_usd,
        },
        review: ReviewReport {
            safety: safety_label(policy.safety),
            prompt_redacted,
        },
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn lookup_app(app_id: &str) -> Result<apps::App, AiError> {
    let apps_dir = std::env::var("COS_APPS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/usr/lib/cos/apps"));
    let discovered = apps::discover(&apps_dir);
    discovered
        .get(app_id)
        .cloned()
        .ok_or_else(|| AiError::UnknownApp(app_id.to_string()))
}

fn parse_verb(s: &str) -> Result<Verb, AiError> {
    match s {
        "ai.chat" => Ok(Verb::AI_CHAT),
        "ai.chat.untrusted" => Ok(Verb::AI_CHAT_UNTRUSTED),
        "ai.embed" => Ok(Verb::AI_EMBED),
        "ai.image.generate" => Ok(Verb::AI_IMAGE_GENERATE),
        "ai.image.analyze" => Ok(Verb::AI_IMAGE_ANALYZE),
        "ai.audio.tts" => Ok(Verb::AI_AUDIO_TTS),
        "ai.audio.stt" => Ok(Verb::AI_AUDIO_STT),
        "ai.vision.analyze" => Ok(Verb::AI_VISION_ANALYZE),
        // ai.bypass is owner-only and not callable via this path.
        other => Err(AiError::UnknownVerb(other.to_string())),
    }
}

fn parse_origin(s: &str) -> Result<PromptOrigin, AiError> {
    match s {
        "trusted" => Ok(PromptOrigin::Trusted),
        "user-input" | "user_input" => Ok(PromptOrigin::UserInput),
        "external-content" | "external_content" => Ok(PromptOrigin::ExternalContent),
        other => Err(AiError::BadOrigin(other.to_string())),
    }
}

fn origin_label(o: &PromptOrigin) -> String {
    match o {
        PromptOrigin::Trusted => "trusted".into(),
        PromptOrigin::UserInput => "user-input".into(),
        PromptOrigin::ExternalContent => "external-content".into(),
    }
}

fn safety_label(s: AiSafety) -> String {
    match s {
        AiSafety::Strict => "strict".into(),
        AiSafety::Standard => "standard".into(),
        AiSafety::Minimal => "minimal".into(),
    }
}

/// Simple `*`-glob match (no character classes). Sufficient for model
/// allowlists like `claude-*`, `gpt-4*`, `*`.
fn matches_any_glob(model: &str, globs: &[String]) -> bool {
    globs.iter().any(|g| match_glob(g, model))
}

fn match_glob(pattern: &str, s: &str) -> bool {
    // Anchored match with `*` = `.*`. Hand-rolled rather than pulling
    // the `glob` crate (which targets paths, not arbitrary strings).
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return parts[0] == s;
    }
    let mut pos = 0usize;
    let bytes = s.as_bytes();
    for (idx, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if idx == 0 {
            if !s.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if idx == parts.len() - 1 {
            return s.ends_with(part) && bytes.len() - part.len() >= pos;
        } else {
            match s[pos..].find(part) {
                Some(off) => pos += off + part.len(),
                None => return false,
            }
        }
    }
    true
}

/// Pick a default model when the app didn't say which one to use. We
/// use the first glob *without* a wildcard, since wildcards are not
/// resolvable on their own; otherwise the call fails and the app must
/// pass `--model`.
fn default_model_from_globs(globs: &[String], app_id: &str) -> Result<String, AiError> {
    globs
        .iter()
        .find(|g| !g.contains('*'))
        .cloned()
        .ok_or_else(|| AiError::NoDefaultModel(app_id.to_string()))
}

fn estimate_units(prompt: &str, max_units: Option<u64>) -> u64 {
    // Cheap: 1 unit ≈ 1 token ≈ 4 chars input + a 128-token reply
    // buffer. The settle step replaces this with the real value.
    let approx = (prompt.chars().count() as u64 / 4) + 128;
    match max_units {
        Some(cap) => approx.min(cap.max(1)),
        None => approx,
    }
}

fn estimate_usd(units: u64) -> f64 {
    // Placeholder pricing: $0.000003 per unit. Real per-model prices
    // arrive in Phase 8 from /etc/cos/ai/prices.yaml.
    (units as f64) * 3e-6
}

fn apply_safety(prompt: &str, safety: AiSafety) -> (String, bool) {
    match safety {
        AiSafety::Minimal => (prompt.to_string(), false),
        AiSafety::Standard | AiSafety::Strict => {
            let redactor = Redactor::default_set();
            let redacted = redactor.redact(prompt);
            let changed = redacted != prompt;
            (redacted, changed)
        }
    }
}

fn build_chat_request(model: &str, user: &str, system: Option<&str>) -> LlmChatRequest {
    LlmChatRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: user.to_string(),
            }],
        }],
        system: system.map(|s| s.to_string()),
        tools: Vec::new(),
        tool_choice: Default::default(),
        max_tokens: Some(1024),
        temperature: None,
        top_p: None,
        stop_sequences: Vec::new(),
        extra: serde_json::Value::Null,
    }
}

// Silence "unused" warnings when this Arc only flows through the
// async pathway above.
#[allow(dead_code)]
fn _ensure_arc_send<T: ?Sized + Send + Sync>(_: &Arc<T>) {}

// ---------------------------------------------------------------------------
// System-agent provider wrapper.
//
// The kernel-resident agent (everything reachable via `cos agent ask`,
// `cos agent chat`, the cos-agent-bridge HTTP service, the doctor,
// the vision/author/delegate paths, …) is NOT an installed app — it
// has no manifest, no `ai` block, no per-app allowlist. But it still
// spends real tokens against real providers, so it deserves the same
// kernel-level oversight that real apps get:
//
//   * `caps::require(Verb::AI_CHAT, name(model))` — defence-in-depth
//     check AND structured audit emission to `caps.jsonl`.
//   * `budget::reserve/settle` against the stable pseudo-app id
//     `system.agent`, capped by `agent.system_budget_monthly_{units,
//     usd}` in `/etc/cos/config.json`.
//
// Implemented as a Provider decorator so call sites need only wrap
// the result of `llm::registry::build(...)`. The wrapper transparently
// forwards every Provider method to the inner; only `chat()` and
// `chat_stream()` get the additional caps/budget pipeline.
// ---------------------------------------------------------------------------

/// Stable pseudo-app id under which the system agent's AI usage is
/// rolled up. Surfaces alongside real apps in `cos agent budget show`.
pub const SYSTEM_AGENT_BUCKET: &str = "system.agent";

/// Wrap an LLM provider so the kernel-resident agent reaches the
/// upstream model through the same caps + budget + audit pipeline
/// real apps use. The wrapper is cheap (single `Arc` indirection)
/// and is the only sanctioned way for `core::agent::*` code to talk
/// to a model — direct `llm::registry::build(...)` consumption
/// should always be followed by this call.
pub fn wrap_for_system(inner: Arc<dyn LlmProvider>) -> Arc<dyn LlmProvider> {
    Arc::new(SystemGatedProvider { inner })
}

struct SystemGatedProvider {
    inner: Arc<dyn LlmProvider>,
}

#[async_trait]
impl LlmProvider for SystemGatedProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn supported_models(&self) -> Vec<String> {
        self.inner.supported_models()
    }

    fn is_configured(&self) -> bool {
        self.inner.is_configured()
    }

    fn engine_info(&self) -> Option<EngineInfo> {
        self.inner.engine_info()
    }

    fn supports_prompt_cache(&self) -> bool {
        self.inner.supports_prompt_cache()
    }

    async fn chat(&self, request: LlmChatRequest) -> LlmResult<LlmChatResponse> {
        let cfg = &config::get().agent;
        let model = request.model.clone();

        // 1. Capability check at the kernel boundary. The session that
        //    spawned the agent loop should hold `ai.chat` already; if
        //    it somehow doesn't (or the caps system was bypassed), we
        //    fail closed here. Also emits one structured record per
        //    call to `caps.jsonl`.
        caps::require(Verb::AI_CHAT, Scope::name(&model))
            .map_err(|d| {
                llm::LlmError::InvalidRequest(format!(
                    "system-agent caps denied for ai.chat: {}",
                    d.to_json()
                ))
            })?;

        // 2. Reserve budget against the system-agent bucket.
        let est_units = estimate_request_units(&request);
        let est_usd = estimate_usd(est_units);
        let cap_units = cfg.system_budget_monthly_units;
        let cap_usd = cfg.system_budget_monthly_usd;

        let mut store = Store::open()
            .map_err(|e| llm::LlmError::Internal(format!("system-agent budget store: {e}")))?;
        store
            .reserve(SYSTEM_AGENT_BUCKET, est_units, est_usd, cap_units, cap_usd)
            .map_err(|e| llm::LlmError::InvalidRequest(format!("system-agent budget: {e}")))?;

        // 3. Delegate to the wrapped provider.
        let resp = self.inner.chat(request).await?;

        // 4. Settle to actuals. Best-effort: settlement errors are
        //    logged but never bubbled — the call has already been
        //    served and refusing to return success here would lie
        //    to the caller about whether the model was invoked.
        let actual_units =
            resp.usage.input_tokens as i64 + resp.usage.output_tokens as i64;
        let delta_units = actual_units - est_units as i64;
        let delta_usd = estimate_usd(actual_units.max(0) as u64) - est_usd;
        if let Err(e) = store.settle(SYSTEM_AGENT_BUCKET, delta_units, delta_usd) {
            tracing::warn!(
                target: "ai.gate",
                "system-agent budget settle failed (delta_units={delta_units}, delta_usd={delta_usd}): {e}",
            );
        }

        Ok(resp)
    }

    async fn chat_stream(
        &self,
        request: LlmChatRequest,
    ) -> LlmResult<BoxStream<'static, LlmResult<StreamEvent>>> {
        // Streaming path: do the caps check and a best-effort
        // up-front reservation, then hand the stream through
        // unmodified. Settlement against streamed actuals is a
        // future enhancement (would require wrapping the stream
        // to capture the terminal `StreamEvent::Done { usage }`
        // and updating the bucket — non-trivial without losing
        // back-pressure semantics, so deferred).
        let cfg = &config::get().agent;
        let model = request.model.clone();

        caps::require(Verb::AI_CHAT, Scope::name(&model))
            .map_err(|d| {
                llm::LlmError::InvalidRequest(format!(
                    "system-agent caps denied for ai.chat: {}",
                    d.to_json()
                ))
            })?;

        let est_units = estimate_request_units(&request);
        let est_usd = estimate_usd(est_units);
        if let Ok(mut store) = Store::open() {
            if let Err(e) = store.reserve(
                SYSTEM_AGENT_BUCKET,
                est_units,
                est_usd,
                cfg.system_budget_monthly_units,
                cfg.system_budget_monthly_usd,
            ) {
                return Err(llm::LlmError::InvalidRequest(format!(
                    "system-agent budget: {e}"
                )));
            }
        }

        self.inner.chat_stream(request).await
    }
}

/// Approximate tokens a request will cost on the input side, using
/// the same 4 chars ≈ 1 token rule of thumb as `estimate_units`. Adds
/// a 256-token output buffer so the reservation isn't blown the
/// moment a non-trivial response comes back.
fn estimate_request_units(req: &LlmChatRequest) -> u64 {
    let mut chars: usize = 0;
    for m in &req.messages {
        for block in &m.content {
            if let ContentBlock::Text { text } = block {
                chars += text.chars().count();
            }
        }
    }
    if let Some(s) = req.system.as_deref() {
        chars += s.chars().count();
    }
    (chars as u64 / 4) + 256
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_exact() {
        assert!(match_glob("claude-3", "claude-3"));
        assert!(!match_glob("claude-3", "claude-4"));
    }

    #[test]
    fn glob_prefix() {
        assert!(match_glob("claude-*", "claude-3.5-sonnet"));
        assert!(!match_glob("claude-*", "gpt-4"));
    }

    #[test]
    fn glob_suffix() {
        assert!(match_glob("*-mini", "gpt-4o-mini"));
        assert!(!match_glob("*-mini", "gpt-4o"));
    }

    #[test]
    fn glob_star_matches_anything() {
        assert!(match_glob("*", "anything-here"));
        assert!(match_glob("*", ""));
    }

    #[test]
    fn default_model_picks_concrete_entry() {
        let globs = vec!["claude-*".to_string(), "gpt-4o".to_string()];
        assert_eq!(default_model_from_globs(&globs, "app").unwrap(), "gpt-4o");
    }

    #[test]
    fn default_model_fails_when_all_globs_wild() {
        let globs = vec!["claude-*".to_string(), "gpt-*".to_string()];
        let err = default_model_from_globs(&globs, "app").unwrap_err();
        assert!(matches!(err, AiError::NoDefaultModel(_)));
    }

    #[test]
    fn parse_verb_known() {
        assert_eq!(parse_verb("ai.chat").unwrap(), Verb::AI_CHAT);
        assert_eq!(
            parse_verb("ai.chat.untrusted").unwrap(),
            Verb::AI_CHAT_UNTRUSTED
        );
    }

    #[test]
    fn parse_verb_rejects_bypass() {
        let err = parse_verb("ai.bypass").unwrap_err();
        assert!(matches!(err, AiError::UnknownVerb(_)));
    }

    #[test]
    fn parse_origin_known() {
        assert_eq!(parse_origin("trusted").unwrap(), PromptOrigin::Trusted);
        assert_eq!(
            parse_origin("external-content").unwrap(),
            PromptOrigin::ExternalContent
        );
    }

    #[test]
    fn estimate_units_respects_cap() {
        let cap = 50;
        assert_eq!(estimate_units("a".repeat(4000).as_str(), Some(cap)), cap);
    }

    #[test]
    fn apply_safety_minimal_is_passthrough() {
        let (out, changed) = apply_safety("hello sk-FAKE", AiSafety::Minimal);
        assert_eq!(out, "hello sk-FAKE");
        assert!(!changed);
    }

    #[test]
    fn apply_safety_strict_redacts() {
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let (out, changed) = apply_safety(&format!("key={secret}"), AiSafety::Strict);
        assert!(changed);
        assert!(!out.contains(secret));
    }
}
