//! AI helper for the stable `chat` / `chat-untrusted` surface.
//!
//! [`chat`] shells out to `cos ai chat --app <id>`. Passing
//! `origin("external-content")` makes the kernel select
//! `ai.chat.untrusted`. Multimodal helper names remain as deprecated,
//! experimental compatibility shims, but are currently unsupported
//! and fail before invoking `cos`.
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
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::{cos_call_json, BridgeError};

// ---------------------------------------------------------------------------
// Response shape
// ---------------------------------------------------------------------------

/// Token / unit accounting for a single call.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub units: u64,
}

/// Snapshot of the app's budget *after* this call.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Budget {
    pub period: String,
    pub units_used: u64,
    pub units_cap: u64,
}

/// Safety / review metadata.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Review {
    pub safety: String,
    pub prompt_redacted: bool,
}

/// A tool call the model proposed but the kernel did **not** execute.
/// Apps decide whether to fulfil it by re-calling [`crate::tools::call`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// Parsed reply from `cos ai chat`. See `wire/v1/ai.schema.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AiResponse {
    pub text: String,
    pub model: String,
    pub provider: String,
    pub verb: String,
    pub usage: Usage,
    pub budget: Budget,
    pub review: Review,
    #[serde(default)]
    pub tool_calls: Vec<ProposedToolCall>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("ai: {0}")]
    InvalidArg(String),

    /// Experimental compatibility shim for a modality that is not stable.
    #[error("{modality}: currently unsupported; only chat/chat-untrusted are stable")]
    UnsupportedModality { modality: &'static str },

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

/// Options for [`chat`]. Compatibility shims retain this type in their
/// signatures but do not inspect it.
#[derive(Debug, Default, Clone)]
pub struct ChatOpts {
    origin: Option<String>,
    max_units: Option<u64>,
    system: Option<String>,
    app_id: Option<String>,
    tools: Option<Vec<String>>,
}

impl ChatOpts {
    pub fn origin(mut self, o: impl Into<String>)    -> Self { self.origin = Some(o.into()); self }
    pub fn max_units(mut self, n: u64)               -> Self { self.max_units = Some(n); self }
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
    dispatch(prompt, opts)
}

/// Deprecated experimental compatibility shim; currently unsupported.
#[deprecated(note = "experimental compatibility shim; currently unsupported")]
pub fn embed(_prompt: &str, _opts: ChatOpts) -> Result<AiResponse, AiError> {
    Err(AiError::UnsupportedModality { modality: "embed" })
}

/// Deprecated experimental compatibility shim; currently unsupported.
#[deprecated(note = "experimental compatibility shim; currently unsupported")]
pub fn image_generate(_prompt: &str, _output: &str, _opts: ChatOpts) -> Result<AiResponse, AiError> {
    Err(AiError::UnsupportedModality { modality: "image.generate" })
}

/// Deprecated experimental compatibility shim; currently unsupported.
#[deprecated(note = "experimental compatibility shim; currently unsupported")]
pub fn image_analyze(_image: &str, _opts: ChatOpts) -> Result<AiResponse, AiError> {
    Err(AiError::UnsupportedModality { modality: "image.analyze" })
}

/// Deprecated experimental compatibility shim; currently unsupported.
#[deprecated(note = "experimental compatibility shim; currently unsupported")]
pub fn vision_analyze(_prompt: &str, _image: &str, _opts: ChatOpts) -> Result<AiResponse, AiError> {
    Err(AiError::UnsupportedModality { modality: "vision.analyze" })
}

/// Deprecated experimental compatibility shim; currently unsupported.
#[deprecated(note = "experimental compatibility shim; currently unsupported")]
pub fn audio_tts(_prompt: &str, _output: &str, _opts: ChatOpts) -> Result<AiResponse, AiError> {
    Err(AiError::UnsupportedModality { modality: "audio.tts" })
}

/// Deprecated experimental compatibility shim; currently unsupported.
#[deprecated(note = "experimental compatibility shim; currently unsupported")]
pub fn audio_stt(_audio: &str, _opts: ChatOpts) -> Result<AiResponse, AiError> {
    Err(AiError::UnsupportedModality { modality: "audio.stt" })
}

/// Deprecated experimental compatibility shim; currently unsupported.
#[deprecated(note = "experimental compatibility shim; currently unsupported")]
pub fn video_generate(_prompt: &str, _output: &str, _opts: ChatOpts) -> Result<AiResponse, AiError> {
    Err(AiError::UnsupportedModality { modality: "video.generate" })
}

/// Deprecated experimental compatibility shim; currently unsupported.
#[deprecated(note = "experimental compatibility shim; currently unsupported")]
pub fn video_analyze(_video: &str, _prompt: Option<&str>, _opts: ChatOpts) -> Result<AiResponse, AiError> {
    Err(AiError::UnsupportedModality { modality: "video.analyze" })
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

struct PrivateInputFile {
    path: PathBuf,
}

impl PrivateInputFile {
    fn new(label: &str, contents: &str) -> Result<Self, AiError> {
        static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "claw-ai-{}-{nonce}-{sequence}-{label}",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .map_err(|error| AiError::Unavailable(format!("create private {label}: {error}")))?;
        if let Err(error) = file
            .write_all(contents.as_bytes())
            .and_then(|_| file.sync_all())
        {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(AiError::Unavailable(format!(
                "write private {label}: {error}"
            )));
        }
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PrivateInputFile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path) {
            eprintln!("claw-os-sdk: failed to remove private AI input file: {error}");
        }
    }
}

fn dispatch(prompt: &str, opts: ChatOpts) -> Result<AiResponse, AiError> {
    let app = opts
        .app_id
        .clone()
        .or_else(|| std::env::var("COS_APP_ID").ok())
        .ok_or_else(|| AiError::InvalidArg(
            "chat: app id is required (pass .app(...) or set COS_APP_ID)".into()
        ))?;

    let origin = opts.origin.clone().unwrap_or_else(|| "trusted".into());

    let mut argv: Vec<OsString> = vec![
        "ai".into(), "chat".into(),
        "--app".into(), app.into(),
        "--origin".into(), origin.into(),
    ];
    let prompt_file = PrivateInputFile::new("prompt", prompt)?;
    argv.push("--prompt-file".into());
    argv.push(prompt_file.path().as_os_str().to_owned());
    if let Some(n) = opts.max_units {
        argv.push("--max-units".into()); argv.push(n.to_string().into());
    }
    let system_file = opts
        .system
        .as_deref()
        .map(|system| PrivateInputFile::new("system", system))
        .transpose()?;
    if let Some(file) = &system_file {
        argv.push("--system-file".into());
        argv.push(file.path().as_os_str().to_owned());
    }
    if let Some(tools) = &opts.tools {
        argv.push("--tools".into());
        argv.push(tools.join(",").into());
    }

    let value = match cos_call_json("ai", "chat", argv) {
        Ok(value) => value,
        Err(BridgeError::AppError {
            message,
            code,
            ..
        }) => {
            let mut payload = serde_json::json!({ "error": &message });
            if let Some(code) = code {
                payload["code"] = serde_json::Value::String(code);
            }
            let message = payload["error"].as_str().unwrap_or("AI request denied");
            return Err(classify_ai_error(message, &payload));
        }
        Err(error) => return Err(AiError::Bridge(error)),
    };
    parse_response(value)
}

fn parse_response(mut value: serde_json::Value) -> Result<AiResponse, AiError> {
    if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
        return Err(classify_ai_error(err, &value));
    }
    crate::generated::validate_ai(&value).map_err(|error| {
        AiError::Unavailable(format!("ai response decode failed: {error}"))
    })?;
    crate::generated::normalize_ai_integers(&mut value);
    let resp: AiResponse =
        serde_json::from_value(value).map_err(|e| {
            AiError::Unavailable(format!("ai response decode failed: {e}"))
        })?;
    Ok(resp)
}

/// Best-effort redaction of an AI request/response payload for use in
/// error messages and Debug output. Replaces the bulky text /
/// embedding / image-bytes fields with their byte counts so the
/// surrounding diagnostic stays useful without leaking content.
fn redact_payload(value: &serde_json::Value) -> serde_json::Value {
    let mut redacted = value.clone();
    if let Some(obj) = redacted.as_object_mut() {
        for key in ["text", "prompt", "messages", "embedding", "embeddings", "data", "raw"] {
            if let Some(v) = obj.get_mut(key) {
                let bytes = serde_json::to_string(v).map(|s| s.len()).unwrap_or(0);
                *v = serde_json::json!({
                    "redacted": true,
                    "approx_bytes": bytes,
                });
            }
        }
    }
    redacted
}

fn classify_ai_error(message: &str, payload: &serde_json::Value) -> AiError {
    let code = payload
        .get("code")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let lower = message.to_lowercase();
    if let Some(c) = code.as_deref() {
        match c.to_ascii_uppercase().as_str() {
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
        // Never embed the raw kernel reply here: AI calls routinely
        // include user text, embedding vectors, or image bytes that
        // logs or error toasts would happily render. Redact to byte
        // counts so the structural diagnostic survives without the
        // payload.
        payload: redact_payload(payload),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/ai.rs"
    ));
}
