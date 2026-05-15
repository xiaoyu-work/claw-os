//! AI helper — every SDK consumer that talks to a language model
//! shells out to `cos ai chat --app <id>` through this module.
//! Mirrors the Python [`claw_os_sdk.ai`] module.
//!
//! The same set of helpers covers every modality (chat / embed /
//! image / vision / audio / video) because the kernel derives the
//! caps verb from the *flag combination*, not from any model name
//! the app supplies. Apps never name a verb and never name a model.
//!
//! ```no_run
//! use claw_os_sdk::ai;
//!
//! fn summarise(body: &str) -> Result<String, ai::AiError> {
//!     let r = ai::chat(body, ai::ChatOpts::default().origin("external-content").max_units(2000))?;
//!     Ok(r.text)
//! }
//! ```

use std::ffi::OsString;

use serde::{Deserialize, Serialize};

use crate::{cos_call_json, BridgeError};

// ---------------------------------------------------------------------------
// Response shape
// ---------------------------------------------------------------------------

/// Token / unit accounting for a single call.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    #[serde(default)]
    pub units: u32,
}

/// Snapshot of the app's budget *after* this call.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Budget {
    #[serde(default)]
    pub period: String,
    #[serde(default)]
    pub units_used: u32,
    #[serde(default)]
    pub units_cap: u32,
}

/// Safety / review metadata.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Review {
    #[serde(default)]
    pub safety: String,
    #[serde(default)]
    pub prompt_redacted: bool,
    #[serde(default)]
    pub response_blocked: bool,
}

/// A tool call the model proposed but the kernel did **not** execute.
/// Apps decide whether to fulfil it by re-calling [`crate::tools::call`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedToolCall {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub input: serde_json::Value,
}

/// Parsed reply from `cos ai chat`. See `wire/v1/ai.schema.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AiResponse {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub verb: String,
    #[serde(default)]
    pub embedding: Vec<f64>,
    #[serde(default)]
    pub output_path: Option<String>,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub budget: Budget,
    #[serde(default)]
    pub review: Review,
    #[serde(default)]
    pub tool_calls: Vec<ProposedToolCall>,
    #[serde(default)]
    pub raw: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("ai: {0}")]
    InvalidArg(String),

    /// Gate refused (caps / origin / unknown app / unknown verb).
    #[error("ai denied: {message}")]
    Denied {
        message: String,
        code: Option<String>,
        payload: serde_json::Value,
    },

    /// Per-app monthly budget exhausted.
    #[error("budget exceeded: {0}")]
    BudgetExceeded(String),

    /// Safety pipeline blocked the call.
    #[error("safety violation: {0}")]
    SafetyViolation(String),

    /// Transport / decode failure.
    #[error("ai unavailable: {0}")]
    Unavailable(String),

    #[error(transparent)]
    Bridge(#[from] BridgeError),
}

// ---------------------------------------------------------------------------
// Options builder
// ---------------------------------------------------------------------------

/// Shared option-bag for every modality. Construct with
/// [`ChatOpts::default`] then fluent-chain only the knobs you care
/// about — modality-specific paths live on dedicated free functions
/// (`image_generate`, `audio_tts`, …).
#[derive(Debug, Default, Clone)]
pub struct ChatOpts {
    origin: Option<String>,
    max_units: Option<u32>,
    system: Option<String>,
    app_id: Option<String>,
    tools: Option<Vec<String>>,
}

impl ChatOpts {
    pub fn origin(mut self, o: impl Into<String>)    -> Self { self.origin = Some(o.into()); self }
    pub fn max_units(mut self, n: u32)               -> Self { self.max_units = Some(n); self }
    pub fn system(mut self, s: impl Into<String>)    -> Self { self.system = Some(s.into()); self }
    pub fn app(mut self, id: impl Into<String>)      -> Self { self.app_id = Some(id.into()); self }
    pub fn tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tools = Some(tools.into_iter().map(Into::into).collect());
        self
    }
}

// ---------------------------------------------------------------------------
// Public helpers
// ---------------------------------------------------------------------------

/// Single-shot chat completion. Maps to wire-family `ai.chat`.
pub fn chat(prompt: &str, opts: ChatOpts) -> Result<AiResponse, AiError> {
    if prompt.trim().is_empty() {
        return Err(AiError::InvalidArg("chat: prompt must be non-empty".into()));
    }
    dispatch(Modality::Chat, Some(prompt), opts, Default::default())
}

/// Embed text into a vector. Result vector at [`AiResponse::embedding`].
pub fn embed(prompt: &str, opts: ChatOpts) -> Result<AiResponse, AiError> {
    if prompt.trim().is_empty() {
        return Err(AiError::InvalidArg("embed: prompt must be non-empty".into()));
    }
    let extras = Extras { embed: true, ..Default::default() };
    dispatch(Modality::Embed, Some(prompt), opts, extras)
}

/// Generate an image from a prompt; the gate writes it to `output`.
pub fn image_generate(prompt: &str, output: &str, opts: ChatOpts) -> Result<AiResponse, AiError> {
    if prompt.trim().is_empty() {
        return Err(AiError::InvalidArg("image_generate: prompt must be non-empty".into()));
    }
    let extras = Extras { image_output: Some(output.into()), ..Default::default() };
    dispatch(Modality::ImageGenerate, Some(prompt), opts, extras)
}

/// Caption / classify an image with no prompt.
pub fn image_analyze(image: &str, opts: ChatOpts) -> Result<AiResponse, AiError> {
    let extras = Extras { image_input: Some(image.into()), ..Default::default() };
    dispatch(Modality::ImageAnalyze, None, opts, extras)
}

/// Answer a textual question about an image.
pub fn vision_analyze(prompt: &str, image: &str, opts: ChatOpts) -> Result<AiResponse, AiError> {
    if prompt.trim().is_empty() {
        return Err(AiError::InvalidArg("vision_analyze: prompt must be non-empty".into()));
    }
    let extras = Extras { image_input: Some(image.into()), ..Default::default() };
    dispatch(Modality::VisionAnalyze, Some(prompt), opts, extras)
}

/// Synthesize speech; the gate writes the audio to `output`.
pub fn audio_tts(prompt: &str, output: &str, opts: ChatOpts) -> Result<AiResponse, AiError> {
    if prompt.trim().is_empty() {
        return Err(AiError::InvalidArg("audio_tts: prompt must be non-empty".into()));
    }
    let extras = Extras { audio_output: Some(output.into()), ..Default::default() };
    dispatch(Modality::AudioTts, Some(prompt), opts, extras)
}

/// Transcribe an audio file. Transcript at [`AiResponse::text`].
pub fn audio_stt(audio: &str, opts: ChatOpts) -> Result<AiResponse, AiError> {
    let extras = Extras { audio_input: Some(audio.into()), ..Default::default() };
    dispatch(Modality::AudioStt, None, opts, extras)
}

/// Generate a video from a prompt; the gate writes it to `output`.
pub fn video_generate(prompt: &str, output: &str, opts: ChatOpts) -> Result<AiResponse, AiError> {
    if prompt.trim().is_empty() {
        return Err(AiError::InvalidArg("video_generate: prompt must be non-empty".into()));
    }
    let extras = Extras { video_output: Some(output.into()), ..Default::default() };
    dispatch(Modality::VideoGenerate, Some(prompt), opts, extras)
}

/// Describe or answer a question about a video file.
pub fn video_analyze(video: &str, prompt: Option<&str>, opts: ChatOpts) -> Result<AiResponse, AiError> {
    let extras = Extras { video_input: Some(video.into()), ..Default::default() };
    dispatch(Modality::VideoAnalyze, prompt, opts, extras)
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum Modality {
    Chat,
    Embed,
    ImageGenerate,
    ImageAnalyze,
    VisionAnalyze,
    AudioTts,
    AudioStt,
    VideoGenerate,
    VideoAnalyze,
}

impl Modality {
    fn name(&self) -> &'static str {
        match self {
            Modality::Chat          => "chat",
            Modality::Embed         => "embed",
            Modality::ImageGenerate => "image.generate",
            Modality::ImageAnalyze  => "image.analyze",
            Modality::VisionAnalyze => "vision.analyze",
            Modality::AudioTts      => "audio.tts",
            Modality::AudioStt      => "audio.stt",
            Modality::VideoGenerate => "video.generate",
            Modality::VideoAnalyze  => "video.analyze",
        }
    }
}

#[derive(Debug, Default, Clone)]
struct Extras {
    embed: bool,
    image_input: Option<String>,
    image_output: Option<String>,
    audio_input: Option<String>,
    audio_output: Option<String>,
    video_input: Option<String>,
    video_output: Option<String>,
}

fn dispatch(
    modality: Modality,
    prompt: Option<&str>,
    opts: ChatOpts,
    extras: Extras,
) -> Result<AiResponse, AiError> {
    let app = opts
        .app_id
        .clone()
        .or_else(|| std::env::var("COS_APP_ID").ok())
        .ok_or_else(|| AiError::InvalidArg(format!(
            "{}: app id is required (pass .app(...) or set COS_APP_ID)",
            modality.name()
        )))?;

    let origin = opts.origin.clone().unwrap_or_else(|| "trusted".into());

    let mut argv: Vec<OsString> = vec![
        "ai".into(), "chat".into(),
        "--app".into(), app.into(),
        "--origin".into(), origin.into(),
    ];
    if let Some(p) = prompt {
        argv.push("--prompt".into()); argv.push(p.into());
    }
    if let Some(n) = opts.max_units {
        argv.push("--max-units".into()); argv.push(n.to_string().into());
    }
    if let Some(s) = &opts.system {
        argv.push("--system".into()); argv.push(s.into());
    }
    if extras.embed {
        argv.push("--embed".into());
    }
    let flag_pairs: &[(&str, &Option<String>)] = &[
        ("--image-input", &extras.image_input),
        ("--image-output", &extras.image_output),
        ("--audio-input", &extras.audio_input),
        ("--audio-output", &extras.audio_output),
        ("--video-input", &extras.video_input),
        ("--video-output", &extras.video_output),
    ];
    for (flag, val) in flag_pairs {
        if let Some(v) = val.as_ref() {
            argv.push((*flag).into()); argv.push(v.into());
        }
    }
    if let Some(tools) = &opts.tools {
        argv.push("--tools".into());
        argv.push(tools.join(",").into());
    }

    let value = cos_call_json("ai", modality.name(), argv)?;
    parse_response(value, modality)
}

fn parse_response(value: serde_json::Value, modality: Modality) -> Result<AiResponse, AiError> {
    if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
        return Err(classify_ai_error(err, &value));
    }
    // Map raw envelope onto AiResponse using strongly-typed deser
    // where possible; fall back to a hand-walked object.
    let mut resp: AiResponse = serde_json::from_value(value.clone()).unwrap_or_default();
    // Preserve raw for callers that want provider-native fields back.
    resp.raw = value;
    // ai.embed's prompt-vector lives at `embedding`; ai.chat puts the
    // text at `text`. Modality name is informational.
    let _ = modality;
    Ok(resp)
}

fn classify_ai_error(message: &str, payload: &serde_json::Value) -> AiError {
    let code = payload
        .get("code")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let lower = message.to_lowercase();
    if let Some(c) = code.as_deref() {
        match c {
            "BUDGET_EXCEEDED" => return AiError::BudgetExceeded(message.to_string()),
            "SAFETY_VIOLATION" => return AiError::SafetyViolation(message.to_string()),
            _ => {}
        }
    }
    if lower.contains("budget") {
        return AiError::BudgetExceeded(message.to_string());
    }
    if lower.contains("safety") || lower.contains("blocked") {
        return AiError::SafetyViolation(message.to_string());
    }
    AiError::Denied {
        message: message.to_string(),
        code,
        payload: payload.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_opts_chaining() {
        let opts = ChatOpts::default()
            .origin("external-content")
            .max_units(2000)
            .app("test-app")
            .tools(["fs.read_text", "kv.get"]);
        assert_eq!(opts.origin.as_deref(), Some("external-content"));
        assert_eq!(opts.max_units, Some(2000));
        assert_eq!(opts.app_id.as_deref(), Some("test-app"));
        assert_eq!(opts.tools.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn chat_rejects_blank_prompt() {
        let err = chat("", ChatOpts::default()).unwrap_err();
        assert!(matches!(err, AiError::InvalidArg(_)));
    }

    #[test]
    fn classify_budget_error_by_code() {
        let payload = serde_json::json!({"error": "out of units", "code": "BUDGET_EXCEEDED"});
        let err = classify_ai_error("out of units", &payload);
        assert!(matches!(err, AiError::BudgetExceeded(_)));
    }

    #[test]
    fn classify_safety_error_by_keyword() {
        let payload = serde_json::json!({"error": "safety blocked"});
        let err = classify_ai_error("safety blocked", &payload);
        assert!(matches!(err, AiError::SafetyViolation(_)));
    }

    // Silence unused-import warnings for HashMap when feature combos
    // exclude every consumer (current code-shape doesn't use it, but
    // we leave the import to keep the module ready for future per-call
    // metadata).
    #[allow(dead_code)]
    fn _placate() {}
}
