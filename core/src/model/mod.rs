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

pub mod bench;
pub mod engines;
pub mod import;
pub mod ipc;
pub mod paths;
pub mod registry;
pub mod runtime;
pub mod service;
pub mod tasks;

use serde_json::{json, Value};

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
        "load" | "unload" | "infer" | "bench" | "rm" => {
            Ok(json!({"status": "not_implemented", "phase": "0.5", "subcommand": command}))
        }
        "status" => Ok(json!({
            "status": "ok",
            "phase": "0.5-skeleton",
            "models_dir": paths::models_dir().display().to_string(),
            "cache_dir": paths::models_cache_dir().display().to_string(),
            "socket": paths::socket_path().display().to_string(),
            "engines_linked": engines::engines_linked(),
            "models_loaded": 0,
        })),
        other => Err(format!(
            "unknown command: {other}. try: list | import | load | unload | infer | status | bench | rm"
        )),
    }
}
