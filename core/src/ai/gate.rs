//! The App–AI Gate. Single entry point Apps use to reach a model.
//!
//! ```text
//!     cos ai chat --app …       apps/_lib/ai.py
//!          │                          │
//!          ▼                          ▼
//!   ai::gate::chat_blocking ─── caps::require(ai.*, name(model))
//!          │                          │
//!          │                          ▼
//!          │                  manifest.ai origin allowlist
//!          │                          │   ▲
//!          │                          │   │ tighten only:
//!          │                          │   │   budget = min(M, U)
//!          │                          │   │   safety = stricter(M, U)
//!          │                          │   │   origins = M ∩ U
//!          │                          │   │
//!          │                          │   └─ user override
//!          │                          │      ($HOME/.config/cos/apps/<id>.json)
//!          │                          │
//!          │                          ▼
//!          │                  user consent (snapshot of manifest AI block)
//!          │                          │   missing  → ConsentRequired
//!          │                          │   drifted  → ConsentStale
//!          │                          │   ($HOME/.config/cos/consents/<id>.json)
//!          │                          ▼
//!          │                  OS-level model from agent.toml
//!          │                          │
//!          │                          ▼
//!          │                  budget::reserve (hard-deny overcap)
//!          │                          │   per-app cap from manifest
//!          │                          │   user cap from
//!          │                          │   $HOME/.config/cos/ai/budget.json
//!          │                          │   (0 = unlimited)
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
//! re-checks the manifest's origin allowlist and uses the OS-owned
//! provider/model from `/etc/cos/agent.toml`. **Apps never pick the
//! model** — the machine owner does, once, in agent config.
//!
//! On top of the manifest, the kernel reads a per-user override file
//! at `$HOME/.config/cos/apps/<id>.json` (written by the Cosmic
//! Settings UI). The override can only **tighten** the manifest:
//! lower the budget, raise the safety profile, shrink the origin
//! allowlist, or kill-switch the App entirely. See
//! [`crate::ai::overrides`] for the merge semantics.
//!
//! Above and orthogonal to overrides, the kernel requires a fresh
//! [`crate::ai::consent`] record — a JSON snapshot of the App's
//! manifest AI block that the user has explicitly approved (via
//! `cos app consent grant <id>` or the equivalent UI). A missing
//! record denies with `consent_required`; a drifted snapshot denies
//! with `consent_stale`. Consent tracks the manifest, not the
//! override, so a developer pushing a looser update always forces a
//! re-prompt regardless of the user's local tightening.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use async_trait::async_trait;
use futures_util::stream::BoxStream;

use crate::agent::llm::{
    self,
    run_log::{self, LlmRunRecord},
    types::{ChatRequest as LlmChatRequest, ChatResponse as LlmChatResponse, ContentBlock,
            EngineInfo, FinishReason, Message, Role, StreamEvent},
    Provider as LlmProvider, Result as LlmResult,
};
use crate::agent::safety::redact::Redactor;
use crate::apps;
use crate::caps::{self, Scope, Verb};
use crate::caps::manifest::{AiSafety, PromptOrigin};
use crate::config;

use super::budget::{BudgetError, Store};
use super::consent;
use super::overrides;
use super::user_budget;

// ---------------------------------------------------------------------------
// Public request / response shapes
// ---------------------------------------------------------------------------

/// One-shot AI request handed in by the CLI / `_lib`. The gate
/// auto-derives the [`Modality`] (and therefore the caps `Verb` to
/// require) from the **shape** of this request — callers never pass a
/// verb directly. See [`Modality::derive`] for the rules.
#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    /// App id this call is attributed to. The gate looks up the app's
    /// manifest to honour its `ai` policy (prompt origin allowlist,
    /// monthly budget, safety profile). The model is **not** under app
    /// control — the OS picks it from `/etc/cos/agent.toml`.
    pub app_id: String,
    /// Where the prompt text originated — `"trusted"`, `"user-input"`,
    /// or `"external-content"`. `external-content` automatically
    /// hardens chat into [`Modality::ChatUntrusted`].
    pub origin: String,
    /// Text portion of the request. Required for chat / embed /
    /// image-generate / audio-tts / video-generate; optional for the
    /// "analyse this artefact" modalities.
    pub prompt: Option<String>,
    pub system: Option<String>,
    pub max_units: Option<u64>,

    // Modality selectors. The gate derives the verb from these — the
    // caller never supplies a verb directly.

    /// True when the caller wants a vector back instead of text.
    /// Mutually exclusive with the other modality selectors.
    pub embed: bool,
    pub image_input: Option<PathBuf>,
    pub image_output: Option<PathBuf>,
    pub audio_input: Option<PathBuf>,
    pub audio_output: Option<PathBuf>,
    pub video_input: Option<PathBuf>,
    pub video_output: Option<PathBuf>,
}

/// Structured envelope returned to apps. Always JSON-serialisable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResult {
    /// The textual result (for chat / image-analyze / audio-stt /
    /// vision-analyze / video-analyze) or an empty string (for
    /// modalities whose primary output is a file or vector).
    pub text: String,
    /// JSON vector for `ai.embed`. Empty for everything else.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedding: Vec<f32>,
    /// Path the gate wrote the binary output to (image-generate,
    /// audio-tts, video-generate). `None` for text-only modalities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<PathBuf>,
    pub model: String,
    pub provider: String,
    /// The caps verb the gate derived for this call. Surfaces in the
    /// JSON envelope so app developers can confirm the gate picked the
    /// modality they intended.
    pub verb: String,
    pub usage: Usage,
    pub budget: BudgetReport,
    pub review: ReviewReport,
}

// ---------------------------------------------------------------------------
// Modality
// ---------------------------------------------------------------------------

/// What the gate is being asked to do. The CLI / `_lib` never names
/// this directly — the gate derives it from the request shape (which
/// of `prompt` / `image_input` / `audio_output` / … are set) and then
/// requires the corresponding caps verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modality {
    /// Text in, text out. Origin is `trusted` or `user-input`.
    Chat,
    /// Text in (origin = `external-content`), text out. Same as
    /// [`Chat`](Modality::Chat) but enforces the hardened verb
    /// `ai.chat.untrusted` so cap grants can scope it separately.
    ChatUntrusted,
    /// Text in, vector out. The text is what the caller wants
    /// embedded; the response carries an `embedding` array.
    Embed,
    /// Text in, image file out.
    ImageGenerate,
    /// Image in, text out (no prompt).
    ImageAnalyze,
    /// Image in + text prompt in, text out.
    VisionAnalyze,
    /// Text in, audio file out.
    AudioTts,
    /// Audio in, text out.
    AudioStt,
    /// Text in, video file out.
    VideoGenerate,
    /// Video in, text out.
    VideoAnalyze,
}

impl Modality {
    /// Infer the modality from the request shape.
    ///
    /// Precedence (first match wins):
    ///
    ///   1. `embed=true`                            → [`Embed`]
    ///   2. `image_output.is_some()`                → [`ImageGenerate`]
    ///   3. `audio_output.is_some()`                → [`AudioTts`]
    ///   4. `video_output.is_some()`                → [`VideoGenerate`]
    ///   5. `image_input.is_some()` + prompt        → [`VisionAnalyze`]
    ///   6. `image_input.is_some()` (no prompt)     → [`ImageAnalyze`]
    ///   7. `audio_input.is_some()`                 → [`AudioStt`]
    ///   8. `video_input.is_some()`                 → [`VideoAnalyze`]
    ///   9. origin = `external-content`             → [`ChatUntrusted`]
    ///  10. otherwise                               → [`Chat`]
    ///
    /// Returns [`AiError::ModalityConflict`] if more than one modality
    /// selector is set (e.g. `image_input` + `audio_input` together).
    pub fn derive(req: &ChatRequest) -> Result<Modality, AiError> {
        // Reject obviously-incoherent combinations up front so apps
        // get a sharp error instead of silent wrong-verb selection.
        let selectors = [
            req.embed,
            req.image_input.is_some(),
            req.image_output.is_some(),
            req.audio_input.is_some(),
            req.audio_output.is_some(),
            req.video_input.is_some(),
            req.video_output.is_some(),
        ];
        let on = selectors.iter().filter(|b| **b).count();
        if on > 1 {
            return Err(AiError::ModalityConflict(
                "multiple modality selectors set; pass at most one of \
                 --embed / --image-input / --image-output / \
                 --audio-input / --audio-output / \
                 --video-input / --video-output"
                    .to_string(),
            ));
        }

        if req.embed {
            return Ok(Modality::Embed);
        }
        if req.image_output.is_some() {
            return Ok(Modality::ImageGenerate);
        }
        if req.audio_output.is_some() {
            return Ok(Modality::AudioTts);
        }
        if req.video_output.is_some() {
            return Ok(Modality::VideoGenerate);
        }
        if req.image_input.is_some() {
            return Ok(if has_prompt(req) {
                Modality::VisionAnalyze
            } else {
                Modality::ImageAnalyze
            });
        }
        if req.audio_input.is_some() {
            return Ok(Modality::AudioStt);
        }
        if req.video_input.is_some() {
            return Ok(Modality::VideoAnalyze);
        }

        // Pure text path. Origin decides hardened-or-not.
        let origin = parse_origin(&req.origin)?;
        Ok(match origin {
            PromptOrigin::ExternalContent => Modality::ChatUntrusted,
            _ => Modality::Chat,
        })
    }

    /// Caps verb required for this modality.
    pub fn verb(self) -> Verb {
        match self {
            Modality::Chat => Verb::AI_CHAT,
            Modality::ChatUntrusted => Verb::AI_CHAT_UNTRUSTED,
            Modality::Embed => Verb::AI_EMBED,
            Modality::ImageGenerate => Verb::AI_IMAGE_GENERATE,
            Modality::ImageAnalyze => Verb::AI_IMAGE_ANALYZE,
            Modality::VisionAnalyze => Verb::AI_VISION_ANALYZE,
            Modality::AudioTts => Verb::AI_AUDIO_TTS,
            Modality::AudioStt => Verb::AI_AUDIO_STT,
            Modality::VideoGenerate => Verb::AI_VIDEO_GENERATE,
            Modality::VideoAnalyze => Verb::AI_VIDEO_ANALYZE,
        }
    }

    /// Lower-snake-case label, used for audit and error messages.
    pub fn label(self) -> &'static str {
        match self {
            Modality::Chat => "chat",
            Modality::ChatUntrusted => "chat_untrusted",
            Modality::Embed => "embed",
            Modality::ImageGenerate => "image_generate",
            Modality::ImageAnalyze => "image_analyze",
            Modality::VisionAnalyze => "vision_analyze",
            Modality::AudioTts => "audio_tts",
            Modality::AudioStt => "audio_stt",
            Modality::VideoGenerate => "video_generate",
            Modality::VideoAnalyze => "video_analyze",
        }
    }

    /// True for modalities the provider trait already supports today
    /// (`provider.chat(...)`). Everything else is gated through the
    /// same caps + budget + audit pipeline but errors out at the
    /// provider step with [`AiError::ModalityNotSupported`] until the
    /// provider trait grows the relevant entry points.
    fn is_chat_like(self) -> bool {
        matches!(self, Modality::Chat | Modality::ChatUntrusted)
    }
}

fn has_prompt(req: &ChatRequest) -> bool {
    req.prompt
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub units: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetReport {
    pub period: String,
    pub units_used: u64,
    pub units_cap: u64,
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

    #[error(
        "app `{0}` is disabled by the user (set `disabled: true` in \
         $HOME/.config/cos/apps/{0}.json)"
    )]
    AppDisabled(String),

    #[error("user override for app `{app}` is malformed: {detail}")]
    BadOverride { app: String, detail: String },

    #[error(
        "app `{app}` has not been approved yet — run `cos app consent grant {app}` \
         to review its AI policy and approve"
    )]
    ConsentRequired { app: String },

    #[error(
        "consent for app `{app}` is stale (changed: {changed:?}) — \
         run `cos app consent grant {app}` to review the new AI policy and re-approve"
    )]
    ConsentStale { app: String, changed: Vec<String> },

    #[error("stored consent for app `{app}` is malformed: {detail}")]
    BadConsent { app: String, detail: String },

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

    #[error("invalid request: {0}")]
    ModalityConflict(String),

    #[error("modality `{0}` is not yet wired to a provider — gate is ready, but no installed model supports it")]
    ModalityNotSupported(&'static str),

    #[error("missing required input for `{modality}`: {field}")]
    MissingInput {
        modality: &'static str,
        field: &'static str,
    },

    #[error("capability denied: {0}")]
    Denied(serde_json::Value),

    #[error("{0}")]
    Budget(#[from] BudgetError),

    #[error(
        "user-level AI budget exceeded: {used} of {cap} units used \
         this period across all apps — raise the cap in Settings → AI \
         → Budget, or set it to 0 to disable the user-level ceiling"
    )]
    UserBudgetExceeded { used: u64, cap: u64 },

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

/// Async entry point. Performs the full gate sequence and emits a
/// per-call record to `<log_dir>/ai.jsonl` for **every** outcome —
/// allowed (status=ok or status=error after the provider runs) and
/// denied (status=denied with a stable `denial_reason` token).
pub async fn chat(req: ChatRequest) -> Result<ChatResult, AiError> {
    let started = std::time::Instant::now();
    // Best-effort verb derivation up-front so audit records carry it
    // even on denial paths. `Modality::derive` runs again inside
    // `chat_inner`; the duplication is cheap and lets the audit
    // attribute denials caused by *non-derivation* errors to the
    // correct verb.
    let verb_label: Option<String> = Modality::derive(&req)
        .ok()
        .map(|m| m.verb().as_str().to_string());
    let result = chat_inner(&req).await;
    let duration_ms = started.elapsed().as_millis() as u64;

    match &result {
        Ok(ok) => {
            let mut rec = LlmRunRecord::from_success(
                &ok.provider,
                &ok.model,
                None,
                FinishReason::Stop,
                &llm::types::Usage {
                    input_tokens: ok.usage.input_tokens,
                    output_tokens: ok.usage.output_tokens,
                    ..Default::default()
                },
                duration_ms,
                None,
            )
            .with_app(&req.app_id);
            if !ok.verb.is_empty() {
                rec = rec.with_verb(&ok.verb);
            }
            run_log::record(&rec);
        }
        Err(err) => {
            let mut rec = LlmRunRecord::from_denial(
                &req.app_id,
                &config::get().agent.model,
                denial_reason_token(err),
                &err.to_string(),
                duration_ms,
                None,
            );
            if let Some(v) = &verb_label {
                rec = rec.with_verb(v);
            }
            run_log::record(&rec);
        }
    }

    result
}

/// Stable, lower-cased machine token classifying a gate denial. Kept
/// in lockstep with the [`AiError`] variants — operators and tests
/// rely on the exact spellings, so don't rename without updating the
/// grep'able doc on [`LlmRunRecord::denial_reason`].
fn denial_reason_token(err: &AiError) -> &'static str {
    match err {
        AiError::UnknownApp(_) => "unknown_app",
        AiError::NoAiPolicy { .. } => "no_ai_policy",
        AiError::AppDisabled(_) => "app_disabled",
        AiError::BadOverride { .. } => "bad_override",
        AiError::ConsentRequired { .. } => "consent_required",
        AiError::ConsentStale { .. } => "consent_stale",
        AiError::BadConsent { .. } => "bad_consent",
        AiError::BadOrigin(_) => "bad_origin",
        AiError::OriginNotAllowed { .. } => "origin_not_allowed",
        AiError::ModalityConflict(_) => "modality_conflict",
        AiError::ModalityNotSupported(_) => "modality_not_supported",
        AiError::MissingInput { .. } => "missing_input",
        AiError::Denied(_) => "caps_denied",
        AiError::Budget(_) => "budget_exceeded",
        AiError::UserBudgetExceeded { .. } => "user_budget_exceeded",
        AiError::Provider(_) => "provider_error",
        AiError::Safety(_) => "safety_block",
        AiError::Internal(_) => "internal",
    }
}

/// Inner gate sequence. Returns the structured error variants so
/// [`chat`] can map them to a stable `denial_reason` for the audit
/// stream before the caller sees them.
async fn chat_inner(req: &ChatRequest) -> Result<ChatResult, AiError> {
    // 1. Locate the app and its AI policy.
    let app = lookup_app(&req.app_id)?;
    let manifest_policy = app
        .manifest
        .ai
        .as_ref()
        .ok_or_else(|| AiError::NoAiPolicy {
            app: req.app_id.clone(),
        })?
        .clone();

    // 1a. Layer the per-user override on top. The user can only
    //     tighten — never loosen — the manifest. A missing override
    //     file is normal; a malformed file aborts the call so the
    //     user notices the problem.
    let user_override = overrides::load(&req.app_id).map_err(|detail| AiError::BadOverride {
        app: req.app_id.clone(),
        detail,
    })?;
    if let Some(o) = &user_override {
        if o.disabled {
            return Err(AiError::AppDisabled(req.app_id.clone()));
        }
    }
    let policy = overrides::apply_to_policy(&manifest_policy, user_override.as_ref());

    // 1b. Require fresh user consent. The user must have explicitly
    //     approved the App's AI ask — consent tracks the **manifest**
    //     policy (not the override), so a developer pushing a looser
    //     manifest update forces a re-prompt even if the user's
    //     override keeps the effective policy tight.
    let stored_consent = consent::load(&req.app_id).map_err(|detail| AiError::BadConsent {
        app: req.app_id.clone(),
        detail,
    })?;
    let stored_consent = stored_consent.ok_or_else(|| AiError::ConsentRequired {
        app: req.app_id.clone(),
    })?;
    if let consent::Freshness::Stale { changed } =
        consent::freshness(&manifest_policy, &stored_consent)
    {
        return Err(AiError::ConsentStale {
            app: req.app_id.clone(),
            changed,
        });
    }

    // 2. Derive the modality from the request shape. This is the
    //    "what is the caller trying to do" decision; everything below
    //    flows from it.
    let modality = Modality::derive(req)?;
    let verb = modality.verb();

    // 3. Parse and validate origin against the manifest's allowlist.
    //    Even non-chat modalities have an origin field — e.g. an app
    //    that summarises external pages might also caption the images
    //    on those pages, and the origin classification still matters.
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

    // 4. Per-modality input validation. The "analyze" verbs require
    //    an input file; the "generate" verbs require a text prompt.
    validate_inputs(req, modality)?;

    // 5. Resolve the model. Apps don't get to pick — the OS owner
    //    configures one provider and one model in
    //    `/etc/cos/agent.toml`, and every app call uses that.
    let cfg = &config::get().agent;
    let model = cfg.model.clone();

    // 6. Capability check at the kernel boundary.
    caps::require(verb, Scope::name(&model))
        .map_err(|d| AiError::Denied(d.to_json()))?;

    // 7. Reserve budget. The estimate depends on modality — token
    //    proxies for chat / embed, flat rates for image / audio /
    //    video. Token counting only — no USD axis.
    let estimated_units = units_for_modality(req, modality);
    let estimated_units = match req.max_units {
        Some(cap) => estimated_units.min(cap.max(1)),
        None => estimated_units,
    };
    let mut store = Store::open().map_err(AiError::Internal)?;
    let _reserved = store.reserve(
        &req.app_id,
        estimated_units,
        policy.budget.monthly_units,
    )?;

    // 7b. Reserve against the per-user aggregate ceiling. This is a
    //     second, independent budget axis: a user who installs many
    //     apps — each with a small per-app cap — can still exhaust
    //     their own monthly token volume. The cap lives in
    //     `$HOME/.config/cos/ai/budget.json` and is opt-in (the file
    //     is missing or `monthly_units == 0` ⇒ no ceiling). If we
    //     accept the per-app reserve but reject here, we MUST roll
    //     back the per-app row so the user isn't billed for a call
    //     that never ran.
    let user_cap = user_budget::load()
        .map_err(AiError::Internal)?
        .monthly_units;
    if user_cap > 0 {
        match store.reserve(user_budget::USER_BUDGET_BUCKET, estimated_units, user_cap) {
            Ok(_) => {}
            Err(BudgetError::OverUnitCap { used, cap, .. }) => {
                let _ = store.settle(&req.app_id, -(estimated_units as i64));
                return Err(AiError::UserBudgetExceeded { used, cap });
            }
            Err(other) => {
                let _ = store.settle(&req.app_id, -(estimated_units as i64));
                return Err(AiError::Budget(other));
            }
        }
    }

    // 8. Apply safety pipeline to the prompt (when present).
    let (prompt_for_provider, prompt_redacted) = match req.prompt.as_deref() {
        Some(p) if !p.is_empty() => {
            let (out, changed) = apply_safety(p, policy.safety);
            (Some(out), changed)
        }
        _ => (None, false),
    };

    // 9. Dispatch by modality. Only chat-like modalities are wired
    //    through to a Provider today; everything else short-circuits
    //    with `ModalityNotSupported` AFTER the caps/budget/safety/
    //    audit machinery has run, so the abuse-detection story works
    //    even before providers grow new entry points.
    if !modality.is_chat_like() {
        // Refund the reservation since we never reached the provider.
        let _ = store.settle(&req.app_id, -(estimated_units as i64));
        if user_cap > 0 {
            let _ = store.settle(
                user_budget::USER_BUDGET_BUCKET,
                -(estimated_units as i64),
            );
        }
        return Err(AiError::ModalityNotSupported(modality.label()));
    }

    let prompt = prompt_for_provider
        .ok_or(AiError::MissingInput {
            modality: modality.label(),
            field: "prompt",
        })?;

    // 10. Build the provider request.
    let provider = llm::registry::build(&cfg.provider, &model, cfg)
        .map_err(|e| AiError::Provider(e.to_string()))?;

    let llm_req = build_chat_request(&model, &prompt, req.system.as_deref());
    let llm_resp = provider
        .chat(llm_req)
        .await
        .map_err(|e| AiError::Provider(e.to_string()))?;

    // 11. Extract the text body.
    let text = llm_resp
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    // 12. Settle the budget against actuals.
    let actual_units =
        llm_resp.usage.input_tokens as i64 + llm_resp.usage.output_tokens as i64;
    let delta_units = actual_units - estimated_units as i64;
    let snapshot = store
        .settle(&req.app_id, delta_units)
        .map_err(AiError::Budget)?;
    // Settle the user-level aggregate row with the same delta so the
    // two ledgers stay in lockstep. Best-effort: settlement errors
    // here are post-call audit noise, not user-visible failures.
    if user_cap > 0 {
        if let Err(e) = store.settle(user_budget::USER_BUDGET_BUCKET, delta_units) {
            eprintln!("ai::gate user-budget settle failed (delta={delta_units}): {e}");
        }
    }

    Ok(ChatResult {
        text,
        embedding: Vec::new(),
        output_path: None,
        model,
        provider: provider.name().to_string(),
        verb: verb.as_str().to_string(),
        usage: Usage {
            input_tokens: llm_resp.usage.input_tokens,
            output_tokens: llm_resp.usage.output_tokens,
            units: actual_units.max(0) as u64,
        },
        budget: BudgetReport {
            period: snapshot.period,
            units_used: snapshot.units_used,
            units_cap: policy.budget.monthly_units,
        },
        review: ReviewReport {
            safety: safety_label(policy.safety),
            prompt_redacted,
        },
    })
}

/// Per-modality required-input check. Runs after origin validation so
/// the "missing prompt" error doesn't mask an earlier policy denial.
fn validate_inputs(req: &ChatRequest, m: Modality) -> Result<(), AiError> {
    use Modality::*;
    let label = m.label();
    let need_prompt = matches!(
        m,
        Chat | ChatUntrusted | Embed | ImageGenerate | AudioTts | VideoGenerate
    );
    if need_prompt && !has_prompt(req) {
        return Err(AiError::MissingInput {
            modality: label,
            field: "prompt",
        });
    }
    let need_input_file = match m {
        ImageAnalyze | VisionAnalyze => req.image_input.is_none(),
        AudioStt => req.audio_input.is_none(),
        VideoAnalyze => req.video_input.is_none(),
        _ => false,
    };
    if need_input_file {
        return Err(AiError::MissingInput {
            modality: label,
            field: "input_file",
        });
    }
    let need_output_file = match m {
        ImageGenerate => req.image_output.is_none(),
        AudioTts => req.audio_output.is_none(),
        VideoGenerate => req.video_output.is_none(),
        _ => false,
    };
    if need_output_file {
        return Err(AiError::MissingInput {
            modality: label,
            field: "output_file",
        });
    }
    Ok(())
}

/// Estimate units the request will charge against the budget. Today
/// this is per-modality stub pricing; the next phase
/// (`prices-loader`) replaces it with `/etc/cos/ai/prices.yaml`.
fn units_for_modality(req: &ChatRequest, m: Modality) -> u64 {
    use Modality::*;
    let prompt_units = req
        .prompt
        .as_deref()
        .map(|p| (p.chars().count() as u64 / 4) + 128)
        .unwrap_or(128);
    match m {
        Chat | ChatUntrusted => prompt_units,
        // Embeddings: input-only, no big response buffer.
        Embed => (req.prompt.as_deref().map(|p| p.chars().count()).unwrap_or(0)
            as u64
            / 4)
            .max(1),
        // Flat rates for binary modalities until prices.yaml lands.
        ImageGenerate => 1_000,
        ImageAnalyze | VisionAnalyze => 100 + prompt_units,
        AudioTts => 10 * req.prompt.as_deref().map(|p| p.chars().count() as u64).unwrap_or(0),
        AudioStt => 100,
        VideoGenerate => 10_000,
        VideoAnalyze => 1_000 + prompt_units,
    }
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
//     `system.agent`, capped by `agent.system_budget_monthly_units`
//     in `/etc/cos/config.json`.
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
        let cap_units = cfg.system_budget_monthly_units;

        let mut store = Store::open()
            .map_err(|e| llm::LlmError::Internal(format!("system-agent budget store: {e}")))?;
        store
            .reserve(SYSTEM_AGENT_BUCKET, est_units, cap_units)
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
        if let Err(e) = store.settle(SYSTEM_AGENT_BUCKET, delta_units) {
            tracing::warn!(
                target: "ai.gate",
                "system-agent budget settle failed (delta_units={delta_units}): {e}",
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
        if let Ok(mut store) = Store::open() {
            if let Err(e) = store.reserve(
                SYSTEM_AGENT_BUCKET,
                est_units,
                cfg.system_budget_monthly_units,
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
    fn parse_origin_known() {
        assert_eq!(parse_origin("trusted").unwrap(), PromptOrigin::Trusted);
        assert_eq!(
            parse_origin("external-content").unwrap(),
            PromptOrigin::ExternalContent
        );
    }

    #[test]
    fn modality_derive_chat_default() {
        let req = ChatRequest {
            app_id: "x".into(),
            origin: "trusted".into(),
            prompt: Some("hi".into()),
            ..Default::default()
        };
        assert_eq!(Modality::derive(&req).unwrap(), Modality::Chat);
    }

    #[test]
    fn modality_derive_chat_untrusted_from_external_origin() {
        let req = ChatRequest {
            app_id: "x".into(),
            origin: "external-content".into(),
            prompt: Some("hi".into()),
            ..Default::default()
        };
        assert_eq!(
            Modality::derive(&req).unwrap(),
            Modality::ChatUntrusted
        );
    }

    #[test]
    fn modality_derive_embed() {
        let req = ChatRequest {
            app_id: "x".into(),
            origin: "trusted".into(),
            prompt: Some("hi".into()),
            embed: true,
            ..Default::default()
        };
        assert_eq!(Modality::derive(&req).unwrap(), Modality::Embed);
    }

    #[test]
    fn modality_derive_image_generate_from_image_output() {
        let req = ChatRequest {
            app_id: "x".into(),
            origin: "trusted".into(),
            prompt: Some("a cat".into()),
            image_output: Some(PathBuf::from("/tmp/out.png")),
            ..Default::default()
        };
        assert_eq!(
            Modality::derive(&req).unwrap(),
            Modality::ImageGenerate
        );
    }

    #[test]
    fn modality_derive_image_analyze_no_prompt() {
        let req = ChatRequest {
            app_id: "x".into(),
            origin: "trusted".into(),
            image_input: Some(PathBuf::from("/tmp/in.png")),
            ..Default::default()
        };
        assert_eq!(
            Modality::derive(&req).unwrap(),
            Modality::ImageAnalyze
        );
    }

    #[test]
    fn modality_derive_vision_analyze_with_prompt() {
        let req = ChatRequest {
            app_id: "x".into(),
            origin: "trusted".into(),
            prompt: Some("describe this".into()),
            image_input: Some(PathBuf::from("/tmp/in.png")),
            ..Default::default()
        };
        assert_eq!(
            Modality::derive(&req).unwrap(),
            Modality::VisionAnalyze
        );
    }

    #[test]
    fn modality_derive_audio_tts() {
        let req = ChatRequest {
            app_id: "x".into(),
            origin: "trusted".into(),
            prompt: Some("hello world".into()),
            audio_output: Some(PathBuf::from("/tmp/out.wav")),
            ..Default::default()
        };
        assert_eq!(Modality::derive(&req).unwrap(), Modality::AudioTts);
    }

    #[test]
    fn modality_derive_audio_stt() {
        let req = ChatRequest {
            app_id: "x".into(),
            origin: "trusted".into(),
            audio_input: Some(PathBuf::from("/tmp/in.wav")),
            ..Default::default()
        };
        assert_eq!(Modality::derive(&req).unwrap(), Modality::AudioStt);
    }

    #[test]
    fn modality_derive_video_generate() {
        let req = ChatRequest {
            app_id: "x".into(),
            origin: "trusted".into(),
            prompt: Some("a sunrise".into()),
            video_output: Some(PathBuf::from("/tmp/out.mp4")),
            ..Default::default()
        };
        assert_eq!(
            Modality::derive(&req).unwrap(),
            Modality::VideoGenerate
        );
    }

    #[test]
    fn modality_derive_video_analyze() {
        let req = ChatRequest {
            app_id: "x".into(),
            origin: "trusted".into(),
            video_input: Some(PathBuf::from("/tmp/in.mp4")),
            ..Default::default()
        };
        assert_eq!(
            Modality::derive(&req).unwrap(),
            Modality::VideoAnalyze
        );
    }

    #[test]
    fn modality_derive_rejects_conflicting_selectors() {
        let req = ChatRequest {
            app_id: "x".into(),
            origin: "trusted".into(),
            image_input: Some(PathBuf::from("/tmp/i.png")),
            audio_input: Some(PathBuf::from("/tmp/a.wav")),
            ..Default::default()
        };
        let err = Modality::derive(&req).unwrap_err();
        assert!(matches!(err, AiError::ModalityConflict(_)));
    }

    #[test]
    fn modality_verbs_cover_every_variant() {
        // Sanity: every variant has a corresponding caps verb. If a
        // future modality is added without wiring caps, this matches
        // statement will fail at compile time.
        let all = [
            Modality::Chat,
            Modality::ChatUntrusted,
            Modality::Embed,
            Modality::ImageGenerate,
            Modality::ImageAnalyze,
            Modality::VisionAnalyze,
            Modality::AudioTts,
            Modality::AudioStt,
            Modality::VideoGenerate,
            Modality::VideoAnalyze,
        ];
        for m in all {
            // verb() always returns; label() always returns. Cover both.
            let _ = m.verb();
            assert!(!m.label().is_empty());
        }
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
