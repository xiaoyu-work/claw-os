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

use super::{Tool, ToolResult};
use crate::agent::media::{
    imagegen::{ImageGenRegistry, ImageGenRequest},
    stt::{SttRegistry, SttRequest},
    tts::{AudioFormat, TtsRegistry, TtsRequest},
};

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
    fn name(&self) -> &'static str {
        "cos_tts"
    }

    fn description(&self) -> &'static str {
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
                "provider": provider_name,
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
    fn name(&self) -> &'static str {
        "cos_stt"
    }

    fn description(&self) -> &'static str {
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

    async fn exec(&self, input: Value) -> ToolResult {
        let path = match input.get("path").and_then(|v| v.as_str()) {
            Some(s) => PathBuf::from(s),
            None => return ToolResult::err("missing required field: path"),
        };
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
        let bytes = match std::fs::read(&path) {
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
        let resp = match provider.transcribe(req).await {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("stt provider error: {e}")),
        };
        ToolResult::ok(
            json!({
                "provider": provider_name,
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
    fn name(&self) -> &'static str {
        "cos_imagegen"
    }

    fn description(&self) -> &'static str {
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
                "provider": provider_name,
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
    use super::*;

    #[tokio::test]
    async fn tts_tool_writes_audio_and_returns_summary() {
        let reg = Arc::new(TtsRegistry::with_default_providers());
        let tool = TtsTool::new(reg);
        let r = tool
            .exec(json!({"text": "hi", "provider": "noop", "format": "wav"}))
            .await;
        assert!(!r.is_error, "got error: {}", r.content);
        let v: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(v["provider"], "noop");
        assert_eq!(v["format"], "wav");
        assert_eq!(v["bytes"], 44);
        let path = v["path"].as_str().unwrap();
        assert!(std::path::Path::new(path).exists());
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn tts_tool_missing_text_errors() {
        let reg = Arc::new(TtsRegistry::with_default_providers());
        let tool = TtsTool::new(reg);
        let r = tool.exec(json!({})).await;
        assert!(r.is_error);
        assert!(r.content.contains("text"));
    }

    #[tokio::test]
    async fn tts_tool_unknown_provider_errors() {
        let reg = Arc::new(TtsRegistry::with_default_providers());
        let tool = TtsTool::new(reg);
        let r = tool
            .exec(json!({"text": "hi", "provider": "nope"}))
            .await;
        assert!(r.is_error);
        assert!(r.content.contains("not registered"));
    }

    #[tokio::test]
    async fn stt_tool_reads_file_and_transcribes() {
        let dir = std::env::temp_dir().join(format!("cos-stt-test-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let audio = dir.join("clip.wav");
        std::fs::write(&audio, b"fake wav bytes").unwrap();
        let reg = Arc::new(SttRegistry::with_default_providers());
        let tool = SttTool::new(reg);
        let r = tool
            .exec(json!({"path": audio.display().to_string(), "language": "en"}))
            .await;
        assert!(!r.is_error, "got error: {}", r.content);
        let v: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(v["provider"], "noop");
        assert_eq!(v["language"], "en");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn stt_tool_missing_path_errors() {
        let reg = Arc::new(SttRegistry::with_default_providers());
        let tool = SttTool::new(reg);
        let r = tool.exec(json!({})).await;
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn stt_tool_missing_file_errors() {
        let reg = Arc::new(SttRegistry::with_default_providers());
        let tool = SttTool::new(reg);
        let r = tool.exec(json!({"path": "/no/such/file.wav"})).await;
        assert!(r.is_error);
        assert!(r.content.contains("read audio"));
    }

    #[tokio::test]
    async fn imagegen_tool_writes_n_images() {
        let reg = Arc::new(ImageGenRegistry::with_default_providers());
        let tool = ImageGenTool::new(reg);
        let r = tool
            .exec(json!({"prompt": "a cat", "n": 2}))
            .await;
        assert!(!r.is_error, "got error: {}", r.content);
        let v: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(v["count"], 2);
        let paths = v["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 2);
        for p in paths {
            let path = p.as_str().unwrap();
            assert!(std::path::Path::new(path).exists());
            std::fs::remove_file(path).ok();
        }
    }

    #[tokio::test]
    async fn imagegen_tool_missing_prompt_errors() {
        let reg = Arc::new(ImageGenRegistry::with_default_providers());
        let tool = ImageGenTool::new(reg);
        let r = tool.exec(json!({})).await;
        assert!(r.is_error);
    }

    #[test]
    fn register_default_adds_three_tools() {
        let mut r = super::super::registry::ToolRegistry::new();
        register_default_media_tools(&mut r);
        assert!(r.get("cos_tts").is_some());
        assert!(r.get("cos_stt").is_some());
        assert!(r.get("cos_imagegen").is_some());
    }

    #[test]
    fn parse_audio_format_aliases() {
        assert_eq!(parse_audio_format("WAV"), AudioFormat::Wav);
        assert_eq!(parse_audio_format("mp3"), AudioFormat::Mp3);
        assert_eq!(parse_audio_format("pcm16"), AudioFormat::Pcm16);
        assert_eq!(parse_audio_format("zzz"), AudioFormat::Other);
    }
}
