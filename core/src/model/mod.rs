//! `cos model` — kernel primitive for local model inference.
//!
//! Hosts a long-running `model-runtime` daemon (via `cos service`) with two
//! engines:
//!
//!   - **ort** (ONNX Runtime) for STT (Whisper), TTS (Piper, KittenTTS),
//!     embedding (BGE/MiniLM), vision (CLIP/SigLIP), image-gen (SD/Flux).
//!   - **llama.cpp** (`llama-cpp-2`) for local LLM (GGUF) — Q14 decision.
//!
//! Models are user-provided files registered via `cos model import <path>`,
//! cached at the system-level path returned by `paths::models_dir()`
//! (default `/var/lib/cos/models/` — see `core/src/paths.rs`).
//!
//! Phase 0.5 ships the skeleton + dispatcher + paths/registry module shells.
//! Engines (`engines::ort`, `engines::llama`) are added when the user supplies
//! the first ONNX/GGUF files.
//!
//! Phase 1.5 added cloud subcommands `cos model embed` and `cos model image`
//! that route through OpenAI-compatible cloud providers (configured via the
//! `[embed]` and `[imagegen]` blocks of `config.json`).

pub mod bench;
pub mod compat;
pub mod engines;
pub mod import;
pub mod ipc;
pub mod paths;
pub mod registry;
pub mod runtime;
pub mod service;
pub mod tasks;

use serde_json::{json, Value};

use tasks::embed::{self, EmbedRequest};
use tasks::imagegen::{self, ImageData, ImageGenRequest};
use tasks::stt::{self, SttMode, SttRequest};
use tasks::tts::{self, TtsRequest};

/// Dispatch a `cos model <command>` invocation.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "list" => Ok(json!({
            "models": registry::list().map_err(|e| e.to_string())?,
            "models_dir": paths::models_dir().display().to_string(),
        })),
        "import" => {
            let path = args.first().cloned().unwrap_or_default();
            if path.is_empty() {
                return Err(
                    "usage: cos model import <path> --as <name> [--task <kind>] [--engine <ort|llama>]"
                        .into(),
                );
            }
            // Phase 0.5: stub. import::import_model() implementation lands with engines.
            Ok(json!({
                "status": "not_implemented",
                "phase": "0.5-skeleton",
                "message": "model import lands with engines (waiting for first user-provided ONNX/GGUF)",
                "received_path": path,
            }))
        }
        "load" => run_load(args),
        "unload" | "infer" | "bench" | "rm" => {
            Ok(json!({"status": "not_implemented", "phase": "0.5", "subcommand": command}))
        }
        "embed" => run_embed(args),
        "image" | "imagegen" => run_imagegen(args),
        "transcribe" | "stt" => run_transcribe(args, SttMode::Transcribe),
        "translate" => run_transcribe(args, SttMode::Translate),
        "speak" | "tts" => run_speak(args),
        "status" => Ok(json!({
            "status": "ok",
            "phase": "0.5-skeleton",
            "models_dir": paths::models_dir().display().to_string(),
            "cache_dir": paths::models_cache_dir().display().to_string(),
            "socket": paths::socket_path().display().to_string(),
            "engines_linked": engines::engines_linked(),
            "models_loaded": 0,
            "embed": embed_status_json(),
            "imagegen": imagegen_status_json(),
            "stt": stt_status_json(),
            "tts": tts_status_json(),
        })),
        other => Err(format!(
            "unknown command: {other}. try: list | import | load | unload | infer | embed | image | transcribe | translate | speak | status | bench | rm"
        )),
    }
}

/// `cos model load <name>@<version>` — validates engine compat and
/// (eventually) loads the model into the daemon. Today the actual
/// load is a stub; what *does* run is the manifest's
/// `requires_engine` check against the active engine + version. This
/// is the enforcement seam for P2.4-B: every load goes through
/// `compat::check_engine_compat` before the engine sees the file.
fn run_load(args: &[String]) -> Result<Value, String> {
    let spec = args
        .first()
        .ok_or_else(|| "usage: cos model load <name>@<version>".to_string())?;
    let manifest = load_model_manifest(spec)?;
    compat::check_engine_compat(&manifest).map_err(|e| e.to_string())?;
    Ok(json!({
        "status": "compat_ok",
        "phase": "0.5-skeleton",
        "model": spec,
        "requires_engine": manifest.requires_engine,
        "gguf_version": manifest.gguf_version,
        "arch": manifest.arch,
        "note": "manifest accepted; native load wiring lands with phase 0.5b",
    }))
}

/// Read and parse a model manifest at `<models_dir>/<name>/<version>/manifest.json`.
/// Used by `run_load` and reusable for any future caller (load,
/// pre-flight tools, MCP probes).
pub(crate) fn load_model_manifest(spec: &str) -> Result<registry::Manifest, String> {
    let (name, version) = spec
        .split_once('@')
        .ok_or_else(|| format!("expected <name>@<version>, got \"{spec}\""))?;
    let manifest_path = paths::models_dir()
        .join(name)
        .join(version)
        .join("manifest.json");
    if !manifest_path.is_file() {
        return Err(format!(
            "no manifest at {} (run `cos model import` first)",
            manifest_path.display()
        ));
    }
    let bytes = std::fs::read(&manifest_path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| format!("manifest parse: {e}"))
}

fn embed_status_json() -> Value {
    let cfg = &crate::config::get().embed;
    let configured = match embed::build_from(cfg) {
        Ok(Some(e)) => e.is_configured(),
        _ => false,
    };
    json!({
        "provider": cfg.provider,
        "model": cfg.model,
        "configured": configured,
    })
}

fn imagegen_status_json() -> Value {
    let cfg = &crate::config::get().imagegen;
    let configured = match imagegen::build_from(cfg) {
        Ok(Some(g)) => g.is_configured(),
        _ => false,
    };
    json!({
        "provider": cfg.provider,
        "model": cfg.model,
        "configured": configured,
    })
}

fn stt_status_json() -> Value {
    let cfg = &crate::config::get().stt;
    let configured = match stt::build_from(cfg) {
        Ok(Some(s)) => s.is_configured(),
        _ => false,
    };
    json!({
        "provider": cfg.provider,
        "model": cfg.model,
        "configured": configured,
    })
}

fn tts_status_json() -> Value {
    let cfg = &crate::config::get().tts;
    let configured = match tts::build_from(cfg) {
        Ok(Some(t)) => t.is_configured(),
        _ => false,
    };
    json!({
        "provider": cfg.provider,
        "model": cfg.model,
        "configured": configured,
    })
}

/// `cos model embed <text> [--model NAME]`
///
/// For multiple inputs, pass `--input <text>` repeatedly or `--inputs-file <path>`
/// (one input per line).
fn run_embed(args: &[String]) -> Result<Value, String> {
    let mut inputs: Vec<String> = Vec::new();
    let mut model_override: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--input" => {
                let v = args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| "--input requires a value".to_string())?;
                inputs.push(v);
                i += 2;
            }
            "--inputs-file" => {
                let path = args
                    .get(i + 1)
                    .ok_or_else(|| "--inputs-file requires a path".to_string())?;
                let body = std::fs::read_to_string(path)
                    .map_err(|e| format!("read {path}: {e}"))?;
                for line in body.lines() {
                    let s = line.trim();
                    if !s.is_empty() {
                        inputs.push(s.to_string());
                    }
                }
                i += 2;
            }
            "--model" => {
                model_override = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--model requires a value".to_string())?,
                );
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            _ => {
                positional.push(args[i].clone());
                i += 1;
            }
        }
    }
    if inputs.is_empty() {
        if positional.is_empty() {
            return Err(
                "usage: cos model embed <text> [--input <text> ...] [--model NAME]".into(),
            );
        }
        // Treat all positional args joined by space as a single input.
        inputs.push(positional.join(" "));
    }

    let mut cfg = crate::config::get().embed.clone();
    if let Some(m) = model_override {
        cfg.model = m;
    }
    let embedder = embed::build_from(&cfg)
        .map_err(|e| format!("embed config: {e}"))?
        .ok_or_else(|| {
            "embedding provider is disabled (provider=\"none\"). Set [embed] in config.json"
                .to_string()
        })?;
    if !embedder.is_configured() {
        return Err(format!(
            "embed provider \"{}\" missing API key (set api_key_credential or api_key_env)",
            cfg.provider
        ));
    }
    // Block on the runtime — the CLI is sync.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let resp = rt
        .block_on(embedder.embed(EmbedRequest { inputs: inputs.clone() }))
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "model": resp.model,
        "dim": resp.dim,
        "count": resp.embeddings.len(),
        "embeddings": resp.embeddings,
        "usage": {
            "prompt_tokens": resp.usage.prompt_tokens,
            "total_tokens": resp.usage.total_tokens,
        },
    }))
}

/// `cos model image <prompt> [--out PATH] [--size 1024x1024] [--quality medium] [--n 1] [--format png]`
///
/// Saves the first generated image to `--out` (default `cos-image.<ext>`)
/// and prints metadata. If the provider returns a URL instead of base64,
/// the URL is returned in the JSON without download.
fn run_imagegen(args: &[String]) -> Result<Value, String> {
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut size: Option<String> = None;
    let mut quality: Option<String> = None;
    let mut format: Option<String> = None;
    let mut out_path: Option<String> = None;
    let mut n: u32 = 1;
    let mut model_override: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--size" => {
                size = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--size requires a value".to_string())?,
                );
                i += 2;
            }
            "--quality" => {
                quality = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--quality requires a value".to_string())?,
                );
                i += 2;
            }
            "--format" => {
                format = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--format requires a value".to_string())?,
                );
                i += 2;
            }
            "--out" => {
                out_path = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--out requires a path".to_string())?,
                );
                i += 2;
            }
            "--n" => {
                n = args
                    .get(i + 1)
                    .ok_or_else(|| "--n requires a value".to_string())?
                    .parse()
                    .map_err(|e: std::num::ParseIntError| e.to_string())?;
                i += 2;
            }
            "--model" => {
                model_override = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--model requires a value".to_string())?,
                );
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            _ => {
                prompt_parts.push(args[i].clone());
                i += 1;
            }
        }
    }
    if prompt_parts.is_empty() {
        return Err(
            "usage: cos model image <prompt> [--size WxH] [--quality medium] [--n 1] [--format png] [--out FILE]"
                .into(),
        );
    }
    let prompt = prompt_parts.join(" ");

    let mut cfg = crate::config::get().imagegen.clone();
    if let Some(m) = model_override {
        cfg.model = m;
    }
    let generator = imagegen::build_from(&cfg)
        .map_err(|e| format!("imagegen config: {e}"))?
        .ok_or_else(|| {
            "image generation is disabled (provider=\"none\"). Set [imagegen] in config.json"
                .to_string()
        })?;
    if !generator.is_configured() {
        return Err(format!(
            "imagegen provider \"{}\" missing API key (set api_key_credential or api_key_env)",
            cfg.provider
        ));
    }

    let request_format = format.clone();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let resp = rt
        .block_on(generator.generate(ImageGenRequest {
            prompt: prompt.clone(),
            size,
            quality,
            n,
            format: request_format,
        }))
        .map_err(|e| e.to_string())?;

    // Choose the on-disk extension.
    let ext = format
        .or_else(|| Some(cfg.default_format.clone()))
        .unwrap_or_else(|| "png".into());
    let mut saved_paths: Vec<String> = Vec::new();
    let mut url_results: Vec<String> = Vec::new();
    for (idx, image) in resp.images.iter().enumerate() {
        match image {
            ImageData::Base64 { data } => {
                let bytes = base64_decode(data).map_err(|e| format!("base64: {e}"))?;
                let path = match (&out_path, idx) {
                    (Some(p), 0) if resp.images.len() == 1 => p.clone(),
                    (Some(p), n) => insert_index_in_path(p, n, &ext),
                    (None, n) => format!("cos-image-{n}.{ext}"),
                };
                std::fs::write(&path, &bytes).map_err(|e| format!("write {path}: {e}"))?;
                saved_paths.push(path);
            }
            ImageData::Url { url } => {
                url_results.push(url.clone());
            }
        }
    }
    Ok(json!({
        "model": resp.model,
        "count": resp.images.len(),
        "saved": saved_paths,
        "urls": url_results,
    }))
}

fn insert_index_in_path(p: &str, idx: usize, ext: &str) -> String {
    let path = std::path::Path::new(p);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("cos-image");
    let parent = path.parent();
    let new_name = format!("{stem}-{idx}.{ext}");
    match parent {
        Some(parent) if !parent.as_os_str().is_empty() => {
            parent.join(new_name).display().to_string()
        }
        _ => new_name,
    }
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| e.to_string())
}

/// `cos model transcribe <audio-path> [--language en] [--prompt "..."]
/// [--format json|text|verbose_json|srt|vtt] [--temperature 0.0]
/// [--model NAME] [--out FILE]`
///
/// `cos model translate <audio-path> [...]` is the same but uses the
/// `/audio/translations` endpoint (always returns English).
fn run_transcribe(args: &[String], mode: SttMode) -> Result<Value, String> {
    let mut audio_path: Option<String> = None;
    let mut language: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut response_format: Option<String> = None;
    let mut temperature: Option<f32> = None;
    let mut model_override: Option<String> = None;
    let mut out_path: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--language" | "--lang" => {
                language = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--language requires a value".to_string())?,
                );
                i += 2;
            }
            "--prompt" => {
                prompt = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--prompt requires a value".to_string())?,
                );
                i += 2;
            }
            "--format" | "--response-format" => {
                response_format = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--format requires a value".to_string())?,
                );
                i += 2;
            }
            "--temperature" => {
                temperature = Some(
                    args.get(i + 1)
                        .ok_or_else(|| "--temperature requires a value".to_string())?
                        .parse()
                        .map_err(|e: std::num::ParseFloatError| e.to_string())?,
                );
                i += 2;
            }
            "--model" => {
                model_override = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--model requires a value".to_string())?,
                );
                i += 2;
            }
            "--out" => {
                out_path = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--out requires a path".to_string())?,
                );
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            _ => {
                if audio_path.is_none() {
                    audio_path = Some(args[i].clone());
                } else {
                    return Err(format!("unexpected positional arg: {}", args[i]));
                }
                i += 1;
            }
        }
    }

    let audio_path = audio_path.ok_or_else(|| {
        "usage: cos model transcribe <audio-path> [--language LL] [--prompt ...] [--format json|text|verbose_json|srt|vtt]"
            .to_string()
    })?;
    let audio = std::fs::read(&audio_path).map_err(|e| format!("read {audio_path}: {e}"))?;
    let filename = std::path::Path::new(&audio_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("audio")
        .to_string();

    let mut cfg = crate::config::get().stt.clone();
    if let Some(m) = model_override {
        cfg.model = m;
    }
    let stt_impl = stt::build_from(&cfg)
        .map_err(|e| format!("stt config: {e}"))?
        .ok_or_else(|| {
            "STT is disabled (provider=\"none\"). Set [stt] in config.json".to_string()
        })?;
    if !stt_impl.is_configured() {
        return Err(format!(
            "stt provider \"{}\" missing API key (set api_key_credential or api_key_env)",
            cfg.provider
        ));
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let resp = rt
        .block_on(stt_impl.transcribe(SttRequest {
            audio,
            filename,
            language,
            prompt,
            response_format,
            temperature,
            mode,
        }))
        .map_err(|e| e.to_string())?;

    if let Some(path) = out_path {
        std::fs::write(&path, &resp.text).map_err(|e| format!("write {path}: {e}"))?;
    }
    Ok(json!({
        "text": resp.text,
        "model": resp.model,
        "language": resp.language,
        "mode": match mode { SttMode::Transcribe => "transcribe", SttMode::Translate => "translate" },
    }))
}

/// `cos model speak <text> --out FILE [--voice alloy] [--format mp3]
/// [--speed 1.0] [--instructions "..."] [--model NAME]`
fn run_speak(args: &[String]) -> Result<Value, String> {
    let mut text_parts: Vec<String> = Vec::new();
    let mut voice: Option<String> = None;
    let mut format: Option<String> = None;
    let mut speed: Option<f32> = None;
    let mut instructions: Option<String> = None;
    let mut model_override: Option<String> = None;
    let mut out_path: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--voice" => {
                voice = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--voice requires a value".to_string())?,
                );
                i += 2;
            }
            "--format" => {
                format = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--format requires a value".to_string())?,
                );
                i += 2;
            }
            "--speed" => {
                speed = Some(
                    args.get(i + 1)
                        .ok_or_else(|| "--speed requires a value".to_string())?
                        .parse()
                        .map_err(|e: std::num::ParseFloatError| e.to_string())?,
                );
                i += 2;
            }
            "--instructions" => {
                instructions = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--instructions requires a value".to_string())?,
                );
                i += 2;
            }
            "--model" => {
                model_override = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--model requires a value".to_string())?,
                );
                i += 2;
            }
            "--out" => {
                out_path = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--out requires a path".to_string())?,
                );
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            _ => {
                text_parts.push(args[i].clone());
                i += 1;
            }
        }
    }
    if text_parts.is_empty() {
        return Err(
            "usage: cos model speak <text> --out FILE [--voice alloy] [--format mp3] [--speed 1.0]"
                .into(),
        );
    }
    let text = text_parts.join(" ");

    let mut cfg = crate::config::get().tts.clone();
    if let Some(m) = model_override {
        cfg.model = m;
    }
    let tts_impl = tts::build_from(&cfg)
        .map_err(|e| format!("tts config: {e}"))?
        .ok_or_else(|| {
            "TTS is disabled (provider=\"none\"). Set [tts] in config.json".to_string()
        })?;
    if !tts_impl.is_configured() {
        return Err(format!(
            "tts provider \"{}\" missing API key (set api_key_credential or api_key_env)",
            cfg.provider
        ));
    }

    let chosen_format = format.clone().unwrap_or_else(|| cfg.default_format.clone());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let resp = rt
        .block_on(tts_impl.synthesize(TtsRequest {
            text,
            voice,
            format,
            speed,
            instructions,
        }))
        .map_err(|e| e.to_string())?;

    let path = out_path.unwrap_or_else(|| format!("cos-speech.{chosen_format}"));
    std::fs::write(&path, &resp.audio).map_err(|e| format!("write {path}: {e}"))?;
    Ok(json!({
        "saved": path,
        "bytes": resp.audio.len(),
        "format": resp.format,
        "model": resp.model,
    }))
}

