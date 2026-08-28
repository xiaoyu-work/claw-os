//! Media tools surfaced to the LLM.
//!
//! Three thin LLM-facing tools (`cos_tts`, `cos_stt`, `cos_imagegen`)
//! that route to the matching media registry. Each tool accepts a
//! `provider` field (defaulting to "noop" when omitted) so the
//! model can target a specific backend, and gracefully reports
//! "provider not registered" rather than failing silently when
//! the backend hasn't been configured yet.
//!
//! Output policy: media bytes are large; we never inline them.
//! Each tool returns a JSON summary (length, format, sample rate,
//! etc.) and writes the raw bytes (TTS audio, generated images)
//! to a deterministic path under `paths::agent_media_outputs_dir()`.
//! The path is included in the JSON so the model can ask the user
//! to open it or pipe it through another tool.
//!
//! STT inputs are loaded from a path the caller supplies (must be
//! within an allowed scope). Inline base64 is rejected to avoid
//! pumping multi-MB audio through the LLM context.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use uuid::Uuid;

use super::exposure::ToolExposure;
use super::{Tool, ToolResult};
use crate::agent::media::{
    imagegen::{ImageGenRegistry, ImageGenRequest},
    stt::{SttRegistry, SttRequest},
    tts::{AudioFormat, TtsRegistry, TtsRequest},
};
use crate::caps::{Cap, Scope, Verb};

fn parse_audio_format(s: &str) -> AudioFormat {
    match s.trim().to_ascii_lowercase().as_str() {
        "wav" => AudioFormat::Wav,
        "mp3" => AudioFormat::Mp3,
        "ogg" => AudioFormat::Ogg,
        "pcm" | "pcm16" => AudioFormat::Pcm16,
        _ => AudioFormat::Other,
    }
}

fn write_output(name: &str, ext: &str, bytes: &[u8]) -> Result<PathBuf, std::io::Error> {
    let dir = crate::paths::agent_media_outputs_dir();
    std::fs::create_dir_all(&dir)?;
    let id = Uuid::new_v4().simple().to_string();
    let path = dir.join(format!("{name}-{id}.{ext}"));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn provider_capabilities(
    verb: Verb,
    names: Vec<String>,
    is_configured: impl Fn(&str) -> bool,
) -> Vec<Cap> {
    names
        .into_iter()
        .filter(|name| is_configured(name))
        .map(|name| Cap::new(verb, Scope::name(name)))
        .collect()
}

fn require_provider(verb: Verb, provider: &str) -> Result<(), ToolResult> {
    crate::caps::require(verb, Scope::name(provider)).map_err(|denial| {
        ToolResult::err(format!(
            "{} denied for provider '{provider}': {denial}",
            verb.as_str()
        ))
    })
}

// =============== TTS tool ===============

pub struct TtsTool {
    registry: Arc<TtsRegistry>,
}

impl TtsTool {
    pub fn new(registry: Arc<TtsRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for TtsTool {
    fn name(&self) -> &str {
        "cos_tts"
    }

    fn description(&self) -> &str {
        "Synthesize speech audio from text. Returns the path to the audio file written under the agent media outputs directory."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "Text to synthesize." },
                "provider": { "type": "string", "description": "TTS provider name. Defaults to 'noop'." },
                "voice": { "type": "string", "description": "Voice id (provider-specific)." },
                "format": { "type": "string", "enum": ["wav", "mp3", "ogg", "pcm"], "description": "Output format." },
                "speed": { "type": "number", "description": "Playback speed multiplier in [0.1, 4.0]." }
            },
            "required": ["text"]
        })
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::always().requiring_any_cap(provider_capabilities(
            Verb::AI_AUDIO_TTS,
            self.registry.names(),
            |name| {
                self.registry
                    .get(name)
                    .is_some_and(|provider| provider.is_configured())
            },
        ))
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let text = match input.get("text").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::err("missing required field: text"),
        };
        let provider_name = input
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("noop")
            .to_string();
        let provider = match self.registry.get(&provider_name) {
            Some(p) => p,
            None => {
                return ToolResult::err(format!("tts provider '{provider_name}' not registered"));
            }
        };
        let mut req = TtsRequest::new(text);
        if let Some(v) = input.get("voice").and_then(|v| v.as_str()) {
            req.voice = Some(v.to_string());
        }
        if let Some(f) = input.get("format").and_then(|v| v.as_str()) {
            req.format = Some(parse_audio_format(f));
        }
        if let Some(s) = input.get("speed").and_then(|v| v.as_f64()) {
            req.speed = Some(s as f32);
        }
        if let Err(error) = req.validate() {
            return ToolResult::err(format!("tts provider error: {error}"));
        }
        let effective_provider = provider.name().to_string();
        if let Err(error) = require_provider(Verb::AI_AUDIO_TTS, &effective_provider) {
            return error;
        }
        let resp = match provider.synthesize(req).await {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("tts provider error: {e}")),
        };
        let path = match write_output("tts", resp.format.extension(), &resp.audio) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(format!("failed to write audio: {e}")),
        };
        ToolResult::ok(
            json!({
                "provider": effective_provider,
                "format": resp.format.extension(),
                "bytes": resp.audio.len(),
                "sample_rate": resp.sample_rate,
                "path": path.display().to_string(),
            })
            .to_string(),
        )
    }
}

// =============== STT tool ===============

pub struct SttTool {
    registry: Arc<SttRegistry>,
}

impl SttTool {
    pub fn new(registry: Arc<SttRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for SttTool {
    fn name(&self) -> &str {
        "cos_stt"
    }

    fn description(&self) -> &str {
        "Transcribe an audio file at the given path. Format is inferred from the extension or supplied via the 'format' field."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Filesystem path to an audio file." },
                "provider": { "type": "string", "description": "STT provider name. Defaults to 'noop'." },
                "language": { "type": "string", "description": "Optional BCP-47 language hint." },
                "format": { "type": "string", "enum": ["wav", "mp3", "ogg", "pcm"], "description": "Audio container/format. Inferred from extension when omitted." }
            },
            "required": ["path"]
        })
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::always()
            .requiring_all_verbs([Verb::FS_READ])
            .requiring_any_cap(provider_capabilities(
                Verb::AI_AUDIO_STT,
                self.registry.names(),
                |name| {
                    self.registry
                        .get(name)
                        .is_some_and(|provider| provider.is_configured())
                },
            ))
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let path = match input.get("path").and_then(|v| v.as_str()) {
            Some(s) => PathBuf::from(s),
            None => return ToolResult::err("missing required field: path"),
        };

        // Path-safety pre-flight. Without this, `cos_stt` is an
        // unguarded "read arbitrary file" primitive that the LLM can
        // point at `/etc/passwd` or `~/.ssh/id_rsa` and exfiltrate
        // the contents via a malicious STT provider (or even via the
        // default `noop` provider's response). Two independent gates:
        //   1. `safety::file_safety::classify` — refuses
        //      credential / system-dir / VCS-internal paths and
        //      resolves symlinks so an attacker can't smuggle.
        //   2. `caps::require(FS_READ, path)` — enforces the
        //      process-wide capability sandbox so the tool can only
        //      read inside paths the operator explicitly granted.
        let classified = crate::agent::safety::file_safety::classify(&path);
        if !classified.is_allow() {
            let cat = classified
                .category()
                .map(|c| c.as_str())
                .unwrap_or("unsafe");
            return ToolResult::err(format!(
                "refusing to read stt audio at {}: classified as {} by file-safety",
                path.display(),
                cat
            ));
        }
        let path_str = path.to_string_lossy().to_string();
        if let Err(denial) = crate::caps::require(
            crate::caps::Verb::FS_READ,
            crate::caps::Scope::path(&path_str),
        ) {
            return ToolResult::err(format!(
                "fs_read denied for stt audio at {path_str}: {denial}"
            ));
        }

        let provider_name = input
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("noop")
            .to_string();
        let provider = match self.registry.get(&provider_name) {
            Some(p) => p,
            None => {
                return ToolResult::err(format!("stt provider '{provider_name}' not registered"));
            }
        };
        // `tokio::fs::read` to avoid blocking the runtime on large
        // audio files — STT inputs can easily run to tens of MB.
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return ToolResult::err(format!("failed to read audio file: {e}")),
        };
        let format = if let Some(f) = input.get("format").and_then(|v| v.as_str()) {
            parse_audio_format(f)
        } else {
            match path.extension().and_then(|e| e.to_str()) {
                Some(ext) => parse_audio_format(ext),
                None => AudioFormat::Other,
            }
        };
        let mut req = SttRequest::new(bytes, format);
        if let Some(l) = input.get("language").and_then(|v| v.as_str()) {
            req.language = Some(l.to_string());
        }
        if let Err(error) = req.validate() {
            return ToolResult::err(format!("stt provider error: {error}"));
        }
        let effective_provider = provider.name().to_string();
        if let Err(error) = require_provider(Verb::AI_AUDIO_STT, &effective_provider) {
            return error;
        }
        let resp = match provider.transcribe(req).await {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("stt provider error: {e}")),
        };
        ToolResult::ok(
            json!({
                "provider": effective_provider,
                "text": resp.text,
                "language": resp.language,
                "segments": resp.segments.len(),
            })
            .to_string(),
        )
    }
}

// =============== ImageGen tool ===============

pub struct ImageGenTool {
    registry: Arc<ImageGenRegistry>,
}

impl ImageGenTool {
    pub fn new(registry: Arc<ImageGenRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for ImageGenTool {
    fn name(&self) -> &str {
        "cos_imagegen"
    }

    fn description(&self) -> &str {
        "Generate images from a text prompt. Returns paths to the rendered images written under the agent media outputs directory."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string" },
                "provider": { "type": "string", "description": "Image-gen provider name. Defaults to 'noop'." },
                "negative_prompt": { "type": "string" },
                "width": { "type": "integer", "minimum": 1 },
                "height": { "type": "integer", "minimum": 1 },
                "steps": { "type": "integer", "minimum": 1 },
                "seed": { "type": "integer" },
                "n": { "type": "integer", "minimum": 1, "maximum": 16 }
            },
            "required": ["prompt"]
        })
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::always().requiring_any_cap(provider_capabilities(
            Verb::AI_IMAGE_GENERATE,
            self.registry.names(),
            |name| {
                self.registry
                    .get(name)
                    .is_some_and(|provider| provider.is_configured())
            },
        ))
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let prompt = match input.get("prompt").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::err("missing required field: prompt"),
        };
        let provider_name = input
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("noop")
            .to_string();
        let provider = match self.registry.get(&provider_name) {
            Some(p) => p,
            None => {
                return ToolResult::err(format!(
                    "imagegen provider '{provider_name}' not registered"
                ));
            }
        };
        let mut req = ImageGenRequest::new(prompt);
        if let Some(s) = input.get("negative_prompt").and_then(|v| v.as_str()) {
            req.negative_prompt = Some(s.to_string());
        }
        if let Some(w) = input.get("width").and_then(|v| v.as_u64()) {
            req.width = Some(w as u32);
        }
        if let Some(h) = input.get("height").and_then(|v| v.as_u64()) {
            req.height = Some(h as u32);
        }
        if let Some(s) = input.get("steps").and_then(|v| v.as_u64()) {
            req.steps = Some(s as u32);
        }
        if let Some(s) = input.get("seed").and_then(|v| v.as_u64()) {
            req.seed = Some(s);
        }
        if let Some(n) = input.get("n").and_then(|v| v.as_u64()) {
            req.n = n as u32;
        }
        if let Err(error) = req.validate() {
            return ToolResult::err(format!("imagegen provider error: {error}"));
        }
        let effective_provider = provider.name().to_string();
        if let Err(error) = require_provider(Verb::AI_IMAGE_GENERATE, &effective_provider) {
            return error;
        }
        let resp = match provider.generate(req).await {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("imagegen provider error: {e}")),
        };
        let mut paths: Vec<String> = Vec::with_capacity(resp.images.len());
        for img in &resp.images {
            match write_output("img", img.format.extension(), &img.bytes) {
                Ok(p) => paths.push(p.display().to_string()),
                Err(e) => return ToolResult::err(format!("failed to write image: {e}")),
            }
        }
        ToolResult::ok(
            json!({
                "provider": effective_provider,
                "model": resp.model,
                "seed_used": resp.seed_used,
                "count": resp.images.len(),
                "paths": paths,
            })
            .to_string(),
        )
    }
}

// =============== Registry helper ===============

/// Register all three media tools (TTS / STT / imagegen) backed by
/// the `with_default_providers` registries. Concrete providers can
/// be added later by callers via shared `Arc<Registry>` cloning.
pub fn register_default_media_tools(reg: &mut super::registry::ToolRegistry) {
    let tts = Arc::new(TtsRegistry::with_default_providers());
    let stt = Arc::new(SttRegistry::with_default_providers());
    let img = Arc::new(ImageGenRegistry::with_default_providers());
    reg.register(Arc::new(TtsTool::new(tts)));
    reg.register(Arc::new(SttTool::new(stt)));
    reg.register(Arc::new(ImageGenTool::new(img)));
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/media.rs"
    ));
}
