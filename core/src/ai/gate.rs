//! The App–AI Gate. Single entry point Apps use to reach a model.
//!
//! ```text
//!     cos ai chat --app …       claw-os-sdk/python/src/claw_os_sdk/ai.py
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

/// One-shot AI request handed in by the CLI / `claw_os_sdk`. The gate
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

    /// Names of App-facing Tools (from `crate::ai::tools::CATALOG`)
    /// that the App wants exposed to the model on this single call.
    /// Each name MUST appear in the App's manifest `ai.tools[]`
    /// allowlist; the gate hard-denies any name that isn't, then
    /// rewrites the survivors as provider-format tool specs. The
    /// model **proposes** calls; the gate returns them as
    /// `tool_calls[]` and never executes them in-line. Empty by
    /// default — most modalities don't need tools.
    pub tools: Vec<String>,
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

    /// Tool calls the model proposed. Empty when no tools were
    /// requested or when the model produced no tool calls. The gate
    /// **never** executes these — Apps inspect them and re-call the
    /// kernel via `cos ai tool <name>` for whichever they choose.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ProposedToolCall>,
}

/// Provider-agnostic shape of a model-proposed tool call. Mirrors
/// `crate::agent::llm::types::ToolCall` but lives in the public gate
/// API so App authors don't have to import internal LLM types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedToolCall {
    /// Provider-issued unique id. Echo it back when the App later
    /// fulfils the call so the model can correlate the result.
    pub id: String,
    /// Tool name from the App-facing catalog (e.g. `"fs.read_text"`).
    pub name: String,
    /// JSON arguments the model wants to pass.
    pub input: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Modality
// ---------------------------------------------------------------------------

/// What the gate is being asked to do. The CLI / `claw_os_sdk` never names
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

    /// True for the stable App-facing modalities. Other selectors are
    /// rejected before app lookup, consent, capability, or budget work.
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

    #[error(
        "tool `{tool}` is not in app `{app}`'s manifest `ai.tools[]` \
         allowlist (declared: {allowed:?}). Add it to the manifest and \
         re-install, or drop it from the `--tools` flag."
    )]
    ToolNotInPolicy {
        app: String,
        tool: String,
        allowed: Vec<String>,
    },

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

    #[error(
        "request unit limit exceeded: conservative maximum is {required} units, \
         but --max-units is {limit}"
    )]
    RequestBudgetExceeded { required: u64, limit: u64 },

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
        AiError::ToolNotInPolicy { .. } => "tool_not_in_policy",
        AiError::MissingInput { .. } => "missing_input",
        AiError::Denied(_) => "caps_denied",
        AiError::Budget(_) => "budget_exceeded",
        AiError::UserBudgetExceeded { .. } => "user_budget_exceeded",
        AiError::RequestBudgetExceeded { .. } => "request_budget_exceeded",
        AiError::Provider(_) => "provider_error",
        AiError::Safety(_) => "safety_block",
        AiError::Internal(_) => "internal",
    }
}

/// RAII guard that owns the per-app + per-user budget reservation
/// for one in-flight AI call. Provider errors refund the reservation;
/// after a provider succeeds, an unexpected drop conservatively charges
/// the estimate so already-spent capacity never becomes reusable.
/// `commit(actual_units)` atomically converts reserved units to actuals.
///
/// Lifecycle:
/// 1. `reserve()` — increments `units_reserved` in both rows.
/// 2. `commit(actual)` — decrements that reservation and increments usage.
/// 3. drop before provider success — releases it; drop after success charges it.
///
/// The user-row is opt-in: if `user_cap == 0` the per-user ledger
/// is not touched at all.
struct BudgetReservation {
    store: Store,
    app_id: String,
    estimated: u64,
    per_app_cap: u64,
    user_cap: u64,
    /// Period that the underlying SQL row was tagged with at
    /// `reserve` time. Re-using this for settle/refund avoids a
    /// month-boundary race where the request started in period N
    /// but settled in N+1, double-billing both periods.
    period: String,
    committed: bool,
    refund_on_drop: bool,
}

fn map_budget_error(error: BudgetError) -> AiError {
    match error {
        BudgetError::OverUnitCap {
            app,
            used,
            cap,
        } if app == user_budget::USER_BUDGET_BUCKET => {
            AiError::UserBudgetExceeded { used, cap }
        }
        other => AiError::Budget(other),
    }
}

impl BudgetReservation {
    fn reserve(
        mut store: Store,
        app_id: String,
        estimated: u64,
        per_app_cap: u64,
        user_cap: u64,
    ) -> Result<Self, AiError> {
        let mut buckets = vec![(app_id.as_str(), per_app_cap)];
        if user_cap > 0 {
            buckets.push((user_budget::USER_BUDGET_BUCKET, user_cap));
        }
        let snap = store
            .reserve_buckets(&buckets, estimated)
            .map_err(map_budget_error)?;
        drop(buckets);
        let period = snap.period;
        Ok(Self {
            store,
            app_id,
            estimated,
            per_app_cap,
            user_cap,
            period,
            committed: false,
            refund_on_drop: true,
        })
    }

    fn retain_estimate_on_drop(&mut self) {
        self.refund_on_drop = false;
    }

    /// Settle the reservation against the actual usage. After this
    /// returns the guard no longer holds a debt — `Drop` will not
    /// refund.
    fn commit(mut self, actual_units: u64) -> Result<crate::ai::budget::Snapshot, AiError> {
        let app_id = self.app_id.clone();
        let mut buckets = vec![(
            app_id.as_str(),
            self.estimated,
            actual_units,
            self.per_app_cap,
        )];
        if self.user_cap > 0 {
            buckets.push((
                user_budget::USER_BUDGET_BUCKET,
                self.estimated,
                actual_units,
                self.user_cap,
            ));
        }
        let result = self.store.settle_reservations(&self.period, &buckets);
        self.committed = true;
        result.map_err(map_budget_error)
    }
}

impl Drop for BudgetReservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let actual_units = if self.refund_on_drop {
            0
        } else {
            self.estimated
        };
        let app_cap = if self.refund_on_drop {
            0
        } else {
            self.per_app_cap
        };
        let app_id = self.app_id.clone();
        let mut buckets = vec![(
            app_id.as_str(),
            self.estimated,
            actual_units,
            app_cap,
        )];
        if self.user_cap > 0 {
            let user_cap = if self.refund_on_drop {
                0
            } else {
                self.user_cap
            };
            buckets.push((
                user_budget::USER_BUDGET_BUCKET,
                self.estimated,
                actual_units,
                user_cap,
            ));
        }
        if let Err(e) = self.store.settle_reservations(&self.period, &buckets) {
            eprintln!(
                "ai::gate budget reservation drop settlement failed (app={}): {e}",
                app_id
            );
        }
    }
}

/// Inner gate sequence. Returns the structured error variants so
/// [`chat`] can map them to a stable `denial_reason` for the audit
/// stream before the caller sees them.
async fn chat_inner(req: &ChatRequest) -> Result<ChatResult, AiError> {
    let modality = Modality::derive(req)?;
    if !modality.is_chat_like() {
        return Err(AiError::ModalityNotSupported(modality.label()));
    }

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

    // 7. Apply safety pipeline to the prompt (when present).
    let (prompt_for_provider, prompt_redacted) = match req.prompt.as_deref() {
        Some(p) if !p.is_empty() => {
            let (out, changed) = apply_safety(p, policy.safety);
            (Some(out), changed)
        }
        _ => (None, false),
    };

    let prompt = prompt_for_provider
        .ok_or(AiError::MissingInput {
            modality: modality.label(),
            field: "prompt",
        })?;

    // 8. Resolve any requested Tools against (1) the App's manifest
    // `ai.tools[]` allowlist and (2) the kernel catalog. The
    // manifest allowlist is the App's declared intent; the catalog
    // check guards against typos and model hallucinations. Both
    // must pass before a Tool is exposed to the model.
    let resolved_tools: Vec<crate::agent::llm::types::Tool> =
        if req.tools.is_empty() {
            Vec::new()
        } else {
            let mut out = Vec::with_capacity(req.tools.len());
            for name in &req.tools {
                if !policy.tools.iter().any(|t| t == name) {
                    return Err(AiError::ToolNotInPolicy {
                        app: req.app_id.clone(),
                        tool: name.clone(),
                        allowed: policy.tools.clone(),
                    });
                }
                let def = crate::ai::tools::lookup(name).ok_or_else(|| {
                    AiError::Provider(format!(
                        "unknown tool requested: {name} (not in catalog)"
                    ))
                })?;
                let schema: serde_json::Value =
                    serde_json::from_str(def.args_schema).unwrap_or(serde_json::json!({}));
                out.push(crate::agent::llm::types::Tool {
                    name: def.name.to_string(),
                    description: def.summary.to_string(),
                    input_schema: schema,
                });
            }
            out
        };

    // 9. Build the exact provider request, then reserve its conservative
    // maximum (serialized input bytes + provider output limit). Reserving
    // the maximum rather than a 4-chars/token guess prevents concurrent
    // calls from collectively overselling the monthly cap.
    let mut llm_req = build_chat_request(
        &model,
        &prompt,
        req.system.as_deref(),
        resolved_tools,
        DEFAULT_APP_MAX_OUTPUT_TOKENS,
    );
    let input_units = estimate_input_units(&llm_req);
    if let Some(limit) = req.max_units {
        let minimum = input_units.saturating_add(1);
        let available_output = limit.checked_sub(input_units).filter(|units| *units > 0);
        let Some(available_output) = available_output else {
            return Err(AiError::RequestBudgetExceeded {
                required: minimum,
                limit,
            });
        };
        llm_req.max_tokens = Some(
            available_output
                .min(DEFAULT_APP_MAX_OUTPUT_TOKENS as u64) as u32,
        );
    }
    let estimated_units = estimate_request_units(&llm_req);
    if let Some(limit) = req.max_units {
        if estimated_units > limit {
            return Err(AiError::RequestBudgetExceeded {
                required: estimated_units,
                limit,
            });
        }
    }

    let provider = llm::registry::build(&cfg.provider, &model, cfg)
        .map_err(|e| AiError::Provider(e.to_string()))?;
    let store = Store::open().map_err(AiError::Internal)?;
    let user_cap = user_budget::load()
        .map_err(AiError::Internal)?
        .monthly_units;
    let mut reservation = BudgetReservation::reserve(
        store,
        req.app_id.clone(),
        estimated_units,
        policy.budget.monthly_units,
        user_cap,
    )?;

    // 10. Invoke the provider. Any provider error drops the guard and
    // refunds the untouched reservation.
    let llm_resp = provider
        .chat(llm_req)
        .await
        .map_err(|e| AiError::Provider(e.to_string()))?;
    reservation.retain_estimate_on_drop();

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

    // 12. Settle the budget against actuals via the reservation
    //     guard. `commit()` consumes the guard, so any subsequent
    //     early return (e.g. response construction) does not double-
    //     refund — there is nothing left to roll back.
    let actual_units = reported_units_or_estimate(
        llm_resp.usage.input_tokens,
        llm_resp.usage.output_tokens,
        estimated_units,
    );
    let snapshot = reservation.commit(actual_units)?;
    if let Some(limit) = req.max_units {
        if actual_units > limit {
            return Err(AiError::RequestBudgetExceeded {
                required: actual_units,
                limit,
            });
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
            units: actual_units,
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
        tool_calls: llm_resp
            .tool_calls
            .into_iter()
            .map(|tc| ProposedToolCall {
                id: tc.id,
                name: tc.name,
                input: tc.input,
            })
            .collect(),
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Cached app lookup. `apps::discover` walks the apps directory and
/// re-parses every manifest on every call — that's fine for a CLI
/// invocation but pathological inside the gate, which is on the hot
/// path of every AI request. We cache the discovered map for up to
/// 60 seconds; this is short enough that `claw-os apps add` is visible
/// within an interactive session and long enough to avoid re-walking
/// the disk on tight request loops.
///
/// `COS_APPS_DIR` is read as part of the cache key so test setups
/// that flip the env between cases (and `setup`/`teardown` blocks)
/// still see fresh results when they override the dir.
fn lookup_app(app_id: &str) -> Result<apps::App, AiError> {
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    struct Entry {
        dir: std::path::PathBuf,
        map: BTreeMap<String, apps::App>,
        fetched_at: Instant,
    }
    static CACHE: OnceLock<Mutex<Option<Entry>>> = OnceLock::new();
    const TTL: Duration = Duration::from_secs(60);

    let apps_dir = std::env::var("COS_APPS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/usr/lib/cos/apps"));

    let cache = CACHE.get_or_init(|| Mutex::new(None));
    // Fast path: read the cache and clone the requested app out.
    if let Ok(guard) = cache.lock() {
        if let Some(entry) = guard.as_ref() {
            if entry.dir == apps_dir && entry.fetched_at.elapsed() < TTL {
                return entry
                    .map
                    .get(app_id)
                    .cloned()
                    .ok_or_else(|| AiError::UnknownApp(app_id.to_string()));
            }
        }
    }

    // Slow path: rebuild the cache. We re-acquire the lock just
    // long enough to swap the new entry in; concurrent calls may
    // each re-discover once, which is benign (the second result
    // overwrites the first with identical data).
    let discovered = apps::discover(&apps_dir);
    let result = discovered
        .get(app_id)
        .cloned()
        .ok_or_else(|| AiError::UnknownApp(app_id.to_string()));
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(Entry {
            dir: apps_dir,
            map: discovered,
            fetched_at: Instant::now(),
        });
    }
    result
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

fn build_chat_request(
    model: &str,
    user: &str,
    system: Option<&str>,
    tools: Vec<crate::agent::llm::types::Tool>,
    max_tokens: u32,
) -> LlmChatRequest {
    LlmChatRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: user.to_string(),
            }],
        }],
        system: system.map(|s| s.to_string()),
        tools,
        tool_choice: Default::default(),
        max_tokens: Some(max_tokens),
        temperature: None,
        top_p: None,
        stop_sequences: Vec::new(),
        extra: serde_json::json!({"_cos_initiator": "agent"}),
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
//     in `~/.config/cos/config.json`.
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

pub fn build_system_provider(
    cfg: &crate::config::AgentConfig,
) -> llm::Result<Arc<dyn LlmProvider>> {
    use crate::agent::llm::provider_chain::{ProviderChain, ProviderSlot};
    use std::collections::BTreeSet;

    if cfg.provider_fallbacks.len() > 8 {
        return Err(llm::LlmError::NotConfigured(format!(
            "provider fallback chain has {} fallbacks; maximum is 8",
            cfg.provider_fallbacks.len()
        )));
    }
    let primary = llm::registry::build(&cfg.provider, &cfg.model, cfg)?;
    let mut slots = vec![ProviderSlot::new(
        wrap_for_system(primary),
        cfg.provider.clone(),
        cfg.model.clone(),
    )];
    let mut identities = BTreeSet::from([(cfg.provider.clone(), cfg.model.clone())]);
    for fallback in &cfg.provider_fallbacks {
        let provider = fallback.provider.trim();
        let model = fallback.model.trim();
        if provider.is_empty() || model.is_empty() {
            return Err(llm::LlmError::NotConfigured(
                "provider fallback entries require non-empty provider and model".to_string(),
            ));
        }
        if provider == "mock" && cfg.provider != "mock" {
            return Err(llm::LlmError::NotConfigured(
                "mock cannot be configured as a production provider fallback".to_string(),
            ));
        }
        if !identities.insert((provider.to_string(), model.to_string())) {
            return Err(llm::LlmError::NotConfigured(format!(
                "duplicate provider fallback: {provider}/{model}"
            )));
        }
        let fallback_cfg = fallback.apply_to(cfg);
        let built = llm::registry::build(provider, model, &fallback_cfg)?;
        slots.push(ProviderSlot::new(
            wrap_for_system(built),
            provider.to_string(),
            model.to_string(),
        ));
    }
    if slots.len() == 1 {
        Ok(slots.remove(0).provider)
    } else {
        Ok(Arc::new(ProviderChain::new(slots)?))
    }
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

    fn effective_provider_name(&self) -> String {
        self.inner.effective_provider_name()
    }

    fn effective_model_name(&self, requested: &str) -> String {
        self.inner.effective_model_name(requested)
    }

    fn fallback_state(&self) -> Option<llm::ProviderFallbackState> {
        self.inner.fallback_state()
    }

    async fn chat(&self, mut request: LlmChatRequest) -> LlmResult<LlmChatResponse> {
        let cfg = &config::get().agent;
        let model = request.model.clone();
        normalize_output_limit(&mut request)?;

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

        // 2. Reserve budget against the system-agent bucket. Provider
        //    errors before a response release the reservation; after a
        //    successful response, any unexpected drop charges the estimate.
        let est_units = estimate_request_units(&request);
        let cap_units = cfg.system_budget_monthly_units;

        let store = Store::open()
            .map_err(|e| llm::LlmError::Internal(format!("system-agent budget store: {e}")))?;
        let mut reservation = SystemBudgetReservation::reserve(store, est_units, cap_units)
            .map_err(|e| llm::LlmError::InvalidRequest(format!("system-agent budget: {e}")))?;

        // 3. Delegate to the wrapped provider. `?` on this line
        //    will drop the reservation, which refunds the full
        //    estimate automatically.
        let resp = self.inner.chat(request).await?;
        reservation.retain_estimate_on_drop();

        // 4. Settle to actuals under the same hard cap. The provider has
        //    already run, but returning success after accounting crossed
        //    the cap would invite retries and hide the enforcement failure.
        let actual_units =
            u64::from(resp.usage.input_tokens) + u64::from(resp.usage.output_tokens);
        reservation.commit_to_actuals(actual_units).map_err(|e| {
            llm::LlmError::InvalidRequest(format!(
                "system-agent budget settlement: {e}"
            ))
        })?;

        Ok(resp)
    }

    async fn chat_stream(
        &self,
        mut request: LlmChatRequest,
    ) -> LlmResult<BoxStream<'static, LlmResult<StreamEvent>>> {
        // Streaming path: do the caps check, reserve, then wrap the
        // returned stream so that:
        //   - on `StreamEvent::Done { usage }` we settle to actuals,
        //   - on early drop (consumer hung up) we charge the conservative
        //     estimate because the provider may already have generated tokens.
        let cfg = &config::get().agent;
        let model = request.model.clone();
        normalize_output_limit(&mut request)?;

        caps::require(Verb::AI_CHAT, Scope::name(&model))
            .map_err(|d| {
                llm::LlmError::InvalidRequest(format!(
                    "system-agent caps denied for ai.chat: {}",
                    d.to_json()
                ))
            })?;

        let est_units = estimate_request_units(&request);
        let store = Store::open().map_err(|e| {
            llm::LlmError::Internal(format!("system-agent budget store: {e}"))
        })?;
        let reservation = SystemBudgetReservation::reserve(
            store,
            est_units,
            cfg.system_budget_monthly_units,
        )
        .map_err(|e| llm::LlmError::InvalidRequest(format!("system-agent budget: {e}")))?;

        let inner_stream = self.inner.chat_stream(request).await?;
        Ok(wrap_system_stream(inner_stream, reservation))
    }
}

/// Drop-guarded reservation for the system-agent budget bucket. Same
/// shape as [`BudgetReservation`] but single-row (no per-user axis).
struct SystemBudgetReservation {
    store: Store,
    estimated: u64,
    cap_units: u64,
    /// Period the reservation row was tagged with; we settle/refund
    /// against this exact period so a long-running streaming call
    /// that crosses a UTC month boundary doesn't end up debiting
    /// two separate rows.
    period: String,
    committed: bool,
    refund_on_drop: bool,
}

impl SystemBudgetReservation {
    fn reserve(
        mut store: Store,
        estimated: u64,
        cap_units: u64,
    ) -> Result<Self, BudgetError> {
        let snap = store.reserve(SYSTEM_AGENT_BUCKET, estimated, cap_units)?;
        Ok(Self {
            store,
            estimated,
            cap_units,
            period: snap.period,
            committed: false,
            refund_on_drop: true,
        })
    }

    /// Settle the bucket from `estimated` to `actual_units` while enforcing
    /// the configured monthly cap. Marks the guard committed even on error
    /// because the upstream provider has already consumed the tokens.
    fn commit_to_actuals(
        &mut self,
        actual_units: u64,
    ) -> Result<crate::ai::budget::Snapshot, BudgetError> {
        let actual_units = if actual_units == 0 {
            self.estimated
        } else {
            actual_units
        };
        let result = self.store.settle_reservation(
            SYSTEM_AGENT_BUCKET,
            &self.period,
            self.estimated,
            actual_units,
            self.cap_units,
        );
        self.committed = true;
        result
    }

    /// Once a streaming provider has accepted the request, an early consumer
    /// disconnect no longer proves that no tokens were spent. Keep the full
    /// conservative reservation instead of refunding it into reusable budget.
    fn retain_estimate_on_drop(&mut self) {
        self.refund_on_drop = false;
    }
}

impl Drop for SystemBudgetReservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let actual_units = if self.refund_on_drop {
            0
        } else {
            self.estimated
        };
        let cap_units = if self.refund_on_drop {
            0
        } else {
            self.cap_units
        };
        if let Err(e) = self.store.settle_reservation(
            SYSTEM_AGENT_BUCKET,
            &self.period,
            self.estimated,
            actual_units,
            cap_units,
        )
        {
            tracing::warn!(
                target: "ai.gate",
                "system-agent budget reservation drop settlement failed: {e}",
            );
        }
    }
}

/// Wraps a streaming chat response so the [`SystemBudgetReservation`]
/// is settled to actuals on `StreamEvent::Done { usage }` and
/// charged at the conservative estimate on early drop. The wrapper carries
/// the reservation in its state and runs `commit_to_actuals` exactly once,
/// the first time it sees a `Done` event.
fn wrap_system_stream(
    inner: BoxStream<'static, LlmResult<StreamEvent>>,
    mut reservation: SystemBudgetReservation,
) -> BoxStream<'static, LlmResult<StreamEvent>> {
    use futures_util::StreamExt;
    reservation.retain_estimate_on_drop();
    let state = std::sync::Arc::new(std::sync::Mutex::new(Some(reservation)));
    let wrapped = inner.map(move |item| {
        if let Ok(StreamEvent::Done { ref usage, .. }) = item {
            if let Some(mut r) = state.lock().ok().and_then(|mut g| g.take()) {
                let actual =
                    u64::from(usage.input_tokens) + u64::from(usage.output_tokens);
                if let Err(e) = r.commit_to_actuals(actual) {
                    return Err(llm::LlmError::InvalidRequest(format!(
                        "system-agent budget settlement: {e}"
                    )));
                }
            }
        }
        item
    });
    Box::pin(wrapped)
}

const DEFAULT_APP_MAX_OUTPUT_TOKENS: u32 = 1024;
const DEFAULT_SYSTEM_MAX_OUTPUT_TOKENS: u32 = 4096;
const REQUEST_TOKEN_OVERHEAD: u64 = 512;

fn reported_units_or_estimate(
    input_tokens: u32,
    output_tokens: u32,
    estimated_units: u64,
) -> u64 {
    let reported = u64::from(input_tokens) + u64::from(output_tokens);
    if reported == 0 {
        estimated_units
    } else {
        reported
    }
}

fn normalize_output_limit(req: &mut LlmChatRequest) -> LlmResult<()> {
    if let serde_json::Value::Object(extra) = &req.extra {
        const RESERVED_OUTPUT_KEYS: &[&str] = &[
            "max_tokens",
            "max_completion_tokens",
            "max_output_tokens",
            "maxOutputTokens",
            "generationConfig",
            "generation_config",
        ];
        if let Some(key) = RESERVED_OUTPUT_KEYS
            .iter()
            .find(|key| extra.contains_key(**key))
        {
            return Err(llm::LlmError::InvalidRequest(format!(
                "provider extra field `{key}` cannot override the budgeted output limit"
            )));
        }
    }
    match req.max_tokens {
        Some(0) => Err(llm::LlmError::InvalidRequest(
            "max_tokens must be greater than zero".to_string(),
        )),
        Some(_) => Ok(()),
        None => {
            req.max_tokens = Some(DEFAULT_SYSTEM_MAX_OUTPUT_TOKENS);
            Ok(())
        }
    }
}

/// Conservative input bound. A tokenizer cannot emit more ordinary text
/// tokens than the UTF-8 bytes that encode the full serialized request;
/// the fixed overhead covers provider-added role and protocol markers.
fn estimate_input_units(req: &LlmChatRequest) -> u64 {
    let mut input_only = req.clone();
    input_only.max_tokens = None;
    serde_json::to_vec(&input_only)
        .map(|encoded| (encoded.len() as u64).saturating_add(REQUEST_TOKEN_OVERHEAD))
        .unwrap_or(u64::MAX)
}

/// Maximum units this request can report if the provider respects the
/// normalized `max_tokens` output limit.
fn estimate_request_units(req: &LlmChatRequest) -> u64 {
    estimate_input_units(req).saturating_add(u64::from(
        req.max_tokens
            .unwrap_or(DEFAULT_SYSTEM_MAX_OUTPUT_TOKENS),
    ))
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

    #[test]
    fn tool_not_in_policy_has_stable_denial_token() {
        let err = AiError::ToolNotInPolicy {
            app: "demo".into(),
            tool: "fs.read_text".into(),
            allowed: vec!["kv.get".into()],
        };
        assert_eq!(denial_reason_token(&err), "tool_not_in_policy");
    }

    #[test]
    fn tool_not_in_policy_display_mentions_app_tool_and_allowed() {
        let err = AiError::ToolNotInPolicy {
            app: "demo".into(),
            tool: "fs.read_text".into(),
            allowed: vec!["kv.get".into()],
        };
        let msg = err.to_string();
        assert!(msg.contains("demo"), "{msg}");
        assert!(msg.contains("fs.read_text"), "{msg}");
        assert!(msg.contains("kv.get"), "{msg}");
        assert!(msg.contains("ai.tools[]"), "{msg}");
    }

    // ---------- BudgetReservation Drop guard (audit fix) ----------

    /// Build a `Store` backed by a private on-disk SQLite file under
    /// a tempdir. Returns the tempdir so the file outlives the
    /// store; dropping it cleans up. We override `COS_DATA_DIR` so
    /// `Store::open()` uses our tempdir instead of the system path.
    fn ephemeral_budget_store_via_tempdir() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("COS_DATA_DIR", dir.path());
        let store = Store::open().expect("open store in tempdir");
        (dir, store)
    }

    /// The `BudgetReservation` Drop guard refunds the reserved
    /// units when it goes out of scope without `commit()`. This is
    /// the audit fix for `ai/gate.rs HIGH`: previously a provider
    /// error between `reserve` and `settle` left the units debited.
    #[test]
    fn budget_refunded_on_provider_error() {
        let (_dir, store) = ephemeral_budget_store_via_tempdir();

        // Take a snapshot of the starting balance so the assertion
        // is independent of any leftover rows in the in-tempdir DB.
        let before = {
            let s = Store::open().unwrap();
            s.current("test.app").unwrap().units_used
        };

        // Reserve 500 units, then drop without committing — mimics
        // a provider error path between `reserve` and `commit`.
        {
            let _r = BudgetReservation::reserve(
                store,
                "test.app".to_string(),
                500,
                10_000,
                0, // no user cap
            )
            .expect("reserve");
            // Reservation is alive here; the row should reflect the debit.
            let probe = Store::open().unwrap();
            let mid = probe.current("test.app").unwrap().units_used;
            assert_eq!(
                mid,
                before + 500,
                "reservation should debit `units_used` while alive"
            );
            // Falling out of scope drops `_r` without calling `commit`.
        }

        // After Drop, the row must be refunded back to `before`.
        let after_store = Store::open().unwrap();
        let after = after_store.current("test.app").unwrap().units_used;
        assert_eq!(
            after, before,
            "BudgetReservation::drop must refund the reservation when commit() was not called"
        );

        std::env::remove_var("COS_DATA_DIR");
    }
}
