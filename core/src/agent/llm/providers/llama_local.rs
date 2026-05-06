//! Local llama.cpp provider — speaks the agent's `Provider` trait, drives
//! a `crate::model::engines::llama_cpp::LlamaEngine` underneath.
//!
//! Compiled unconditionally. Engine availability is decided at runtime
//! based on whether `cos engine` has installed an active llama-cpp
//! version. When no engine is installed the provider's
//! `is_configured()` returns false and `chat()` returns a clear
//! `LlmError::NotConfigured` pointing at `cos engine update llama-cpp`.
//!
//! The model identifier the provider receives doubles as the GGUF file
//! path. The expected formats are:
//!
//!   - bare path: `/var/lib/cos/models/llama3-8b/v1/model.gguf`
//!   - `llama_local:` prefix: `llama_local:/path/to/model.gguf`
//!
//! Both forms are accepted to keep CLI ergonomics flexible — `cos agent
//! ask --provider llama_local --model /path/to.gguf` or the prefix form.

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crate::agent::llm::{
    ChatRequest, ChatResponse, ContentBlock, EngineInfo, FinishReason, LlmError, Provider,
    Result, StreamEvent, Usage,
};
use crate::config::AgentConfig;
use crate::model::engines::llama_cpp::{self as llama_engine, LlamaConfig, LlamaEngine};

pub const PROVIDER_NAME: &str = "llama_local";

/// Strip the optional `llama_local:` prefix from a model spec.
fn extract_model_path(model: &str) -> PathBuf {
    PathBuf::from(model.strip_prefix("llama_local:").unwrap_or(model))
}

pub struct LlamaLocalProvider {
    /// What the user passed in — kept for `name()` / status output.
    model_spec: String,
    cfg: LlamaConfig,
    /// Lazily constructed: the actual engine. None until first chat call
    /// (or until something forces eager init). We keep it lazy because
    /// loading a model is expensive and `cos agent status` should not
    /// trigger it.
    engine: tokio::sync::Mutex<Option<Arc<LlamaEngine>>>,
    /// Captured once the engine is first loaded successfully. Read by
    /// the sync `Provider::engine_info()` impl, which can't take an
    /// async mutex. `OnceLock` (sync) avoids needing `block_on` in
    /// the audit path.
    loaded_info: OnceLock<EngineInfo>,
}

// Methods are reached via `dyn Provider` from the runtime when the agent's
// configured provider is `llama_local`; the binary build with the default
// (mock) provider can't see those call sites, so suppress the warnings.
#[allow(dead_code)]
impl LlamaLocalProvider {
    pub fn new(model: &str, agent_cfg: &AgentConfig) -> Self {
        let path = extract_model_path(model);
        let cfg = LlamaConfig {
            model_path: path,
            n_ctx: 0,
            n_threads: 0,
            n_gpu_layers: 0,
            max_tokens: agent_cfg.max_tokens,
            temperature: agent_cfg.temperature,
        };
        Self {
            model_spec: model.to_string(),
            cfg,
            engine: tokio::sync::Mutex::new(None),
            loaded_info: OnceLock::new(),
        }
    }

    /// Validate both the engine runtime is installed AND the model
    /// file exists. Cheap — no library load, no FFI calls.
    fn config_is_usable(&self) -> bool {
        llama_engine::is_installed() && llama_engine::model_path_is_usable(&self.cfg.model_path)
    }

    async fn ensure_engine(&self) -> Result<Arc<LlamaEngine>> {
        let mut slot = self.engine.lock().await;
        if let Some(eng) = slot.as_ref() {
            return Ok(eng.clone());
        }

        if !llama_engine::is_installed() {
            return Err(LlmError::NotConfigured(
                "llama_local provider: no llama-cpp engine installed. \
                 Run `cos engine update llama-cpp` to download the latest \
                 prebuilt release, or `cos engine install llama-cpp@<ver> \
                 --from <archive>` for an offline install."
                    .into(),
            ));
        }

        let cfg = self.cfg.clone();
        // Engine construction loads the dynamic library on first call,
        // which can take a moment (few ms to a few hundred ms on
        // Windows with CUDA bits). Push to the blocking pool.
        let engine = tokio::task::spawn_blocking(move || LlamaEngine::new(cfg))
            .await
            .map_err(|e| LlmError::Internal(format!("spawn_blocking join failed: {e}")))?
            .map_err(|e| LlmError::NotConfigured(format!("llama_cpp engine: {e}")))?;
        // Capture the version that ACTUALLY loaded for the audit
        // trail. The engine_pkg registry can race with this loaded
        // singleton (e.g. `cos engine activate <new>` after the
        // daemon is up — the new version doesn't take effect until
        // restart). Reading from the runtime is the truth.
        if let Some(version) = engine.engine_version() {
            let _ = self.loaded_info.set(EngineInfo {
                name: llama_engine::PKG_ENGINE_NAME.to_string(),
                version,
            });
        }
        let arc = Arc::new(engine);
        *slot = Some(arc.clone());
        Ok(arc)
    }
}

#[async_trait]
impl Provider for LlamaLocalProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn supported_models(&self) -> Vec<String> {
        // Local models are arbitrary GGUF files — we report the configured
        // spec so `cos agent status` shows what's wired without pretending
        // to enumerate the filesystem.
        vec![self.model_spec.clone()]
    }

    fn is_configured(&self) -> bool {
        self.config_is_usable()
    }

    fn engine_info(&self) -> Option<EngineInfo> {
        self.loaded_info.get().cloned()
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let engine = self.ensure_engine().await?;

        let prompt = llama_engine::render_messages_as_prompt(
            request.system.as_deref(),
            &request.messages,
        );

        // Engine.generate is the Phase 0.5 boundary: returns a clear
        // "pending" error when the decode loop hasn't been wired yet.
        match engine.generate(&prompt).await {
            Ok(gen) => Ok(ChatResponse {
                model: self.model_spec.clone(),
                content: vec![ContentBlock::Text { text: gen.text }],
                tool_calls: Vec::new(),
                finish_reason: match gen.stop_reason {
                    llama_engine::StopReason::Eos => FinishReason::Stop,
                    llama_engine::StopReason::MaxTokens => FinishReason::Length,
                    llama_engine::StopReason::StopSequence => FinishReason::Stop,
                    llama_engine::StopReason::Other => FinishReason::Other,
                },
                usage: Usage {
                    input_tokens: gen.prompt_tokens,
                    output_tokens: gen.completion_tokens,
                    ..Default::default()
                },
            }),
            Err(e) => Err(LlmError::Internal(format!("llama_cpp engine: {e}"))),
        }
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        // No native streaming yet — fall back to a single-shot stream
        // wrapping the buffered chat() response, identical to mock.rs.
        let response = self.chat(request).await?;
        let finish = response.finish_reason;
        let usage = response.usage.clone();
        let events: Vec<std::result::Result<StreamEvent, LlmError>> = vec![
            Ok(StreamEvent::Message(response)),
            Ok(StreamEvent::Done { finish, usage }),
        ];
        Ok(stream::iter(events).boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::Message;

    fn req(model: &str, text: &str) -> ChatRequest {
        ChatRequest {
            model: model.into(),
            messages: vec![Message::user_text(text)],
            system: Some("you are a local model".into()),
            tools: vec![],
            tool_choice: Default::default(),
            max_tokens: Some(64),
            temperature: Some(0.5),
            top_p: None,
            stop_sequences: vec![],
            extra: serde_json::Value::Null,
        }
    }

    #[test]
    fn extract_model_path_strips_prefix() {
        assert_eq!(
            extract_model_path("llama_local:/tmp/m.gguf"),
            PathBuf::from("/tmp/m.gguf")
        );
        assert_eq!(extract_model_path("/abs/m.gguf"), PathBuf::from("/abs/m.gguf"));
        assert_eq!(extract_model_path("model.gguf"), PathBuf::from("model.gguf"));
    }

    #[test]
    fn name_is_stable() {
        let p = LlamaLocalProvider::new("/tmp/x.gguf", &AgentConfig::default());
        assert_eq!(p.name(), "llama_local");
    }

    #[test]
    fn supported_models_echoes_spec() {
        let p = LlamaLocalProvider::new("llama_local:/tmp/x.gguf", &AgentConfig::default());
        let m = p.supported_models();
        assert_eq!(m, vec!["llama_local:/tmp/x.gguf".to_string()]);
    }

    #[test]
    fn is_configured_false_when_path_missing() {
        let p = LlamaLocalProvider::new(
            "/this/path/should/not/exist.gguf",
            &AgentConfig::default(),
        );
        assert!(!p.is_configured());
    }

    /// `is_configured()` ANDs file presence with engine-installed-on-disk.
    /// Without an installed engine, even a real model file should not
    /// flip it on. This test pins the engines_dir to an empty temp
    /// directory so the host's actual install (if any) doesn't leak in.
    #[test]
    fn is_configured_requires_installed_engine() {
        let tmp_engines = tempfile::tempdir().unwrap();
        crate::engine_pkg::paths::set_engines_dir_override(Some(
            tmp_engines.path().to_path_buf(),
        ));

        let tmp_gguf = std::env::temp_dir().join(format!(
            "cos-llama-prov-{}-{}.gguf",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&tmp_gguf, b"placeholder").unwrap();
        let spec = tmp_gguf.to_string_lossy().to_string();
        let p = LlamaLocalProvider::new(&spec, &AgentConfig::default());

        assert!(
            !p.is_configured(),
            "no engine installed -> not configured even with model file"
        );

        let _ = std::fs::remove_file(&tmp_gguf);
        crate::engine_pkg::paths::set_engines_dir_override(None);
    }

    /// With both an engine *and* a model file installed, the provider
    /// reports configured. This is the "happy path" Phase 0.5 status
    /// will display once a user has run `cos engine update llama-cpp`.
    #[test]
    fn is_configured_true_when_engine_and_model_present() {
        let tmp_engines = tempfile::tempdir().unwrap();
        crate::engine_pkg::paths::set_engines_dir_override(Some(
            tmp_engines.path().to_path_buf(),
        ));

        // Stand up a fake "installed engine".
        let lib_dir = tmp_engines.path().join("llama-cpp/v0/lib");
        std::fs::create_dir_all(&lib_dir).unwrap();
        let lib_name = if cfg!(target_os = "windows") {
            "llama.dll"
        } else if cfg!(target_os = "macos") {
            "libllama.dylib"
        } else {
            "libllama.so"
        };
        std::fs::write(lib_dir.join(lib_name), b"placeholder").unwrap();
        let json = serde_json::json!({
            "version": 1,
            "engines": {
                "llama-cpp": {
                    "active": "v0",
                    "previous": "",
                    "installed": [{"version": "v0", "installed_at": "2026-01-01T00:00:00Z", "bytes": 0, "source": "", "sha256": ""}],
                    "pinned": false,
                    "channel": "release",
                    "accelerator": "",
                    "source": ""
                }
            }
        });
        std::fs::write(
            tmp_engines.path().join("engines.json"),
            serde_json::to_vec_pretty(&json).unwrap(),
        )
        .unwrap();

        // And a fake GGUF.
        let gguf = std::env::temp_dir().join(format!(
            "cos-llama-prov-happy-{}-{}.gguf",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&gguf, b"placeholder").unwrap();
        let spec = gguf.to_string_lossy().to_string();
        let p = LlamaLocalProvider::new(&spec, &AgentConfig::default());

        assert!(p.is_configured());

        let _ = std::fs::remove_file(&gguf);
        crate::engine_pkg::paths::set_engines_dir_override(None);
    }

    /// With no engine installed, `chat()` surfaces `NotConfigured` so
    /// the user gets a clear pointer to `cos engine update`.
    #[tokio::test]
    async fn chat_without_engine_returns_not_configured() {
        let tmp_engines = tempfile::tempdir().unwrap();
        // The provider runs `is_installed()` from a tokio worker via
        // `spawn_blocking`. The thread-local override lives on the
        // current thread only, so the worker would see the host's real
        // engines dir. Bypass by calling chat() — its initial path
        // through `ensure_engine().await` runs on the current task
        // until it hits `spawn_blocking`. The `is_installed()` check
        // happens BEFORE `spawn_blocking`, so the override applies.
        crate::engine_pkg::paths::set_engines_dir_override(Some(
            tmp_engines.path().to_path_buf(),
        ));

        let p = LlamaLocalProvider::new("/tmp/anything.gguf", &AgentConfig::default());
        let err = p.chat(req("/tmp/anything.gguf", "hi")).await.unwrap_err();
        match err {
            LlmError::NotConfigured(msg) => {
                assert!(
                    msg.contains("llama-cpp") || msg.contains("cos engine"),
                    "unexpected message: {msg}"
                );
            }
            other => panic!("expected NotConfigured, got {other:?}"),
        }

        crate::engine_pkg::paths::set_engines_dir_override(None);
    }
}
