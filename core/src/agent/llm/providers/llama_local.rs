//! Local llama.cpp provider — speaks the agent's `Provider` trait, drives
//! a `crate::model::engines::llama_cpp::LlamaEngine` underneath.
//!
//! Compiled in both feature modes so the registry / status output is
//! consistent. When the `llama_cpp` feature is OFF the provider still
//! exists as a stub: `is_configured()` returns false and `chat()` returns
//! a clear `LlmError::NotConfigured` pointing at the missing feature.
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
use std::sync::Arc;

use crate::agent::llm::{
    ChatRequest, ChatResponse, ContentBlock, FinishReason, LlmError, Provider, Result,
    StreamEvent, Usage,
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
        }
    }

    /// Validate the configured model file exists. Used by `is_configured`
    /// and to give the user an explicit early failure before starting a
    /// long inference call. Cheap — no FFI.
    fn config_is_usable(&self) -> bool {
        llama_engine::IS_LINKED && llama_engine::model_path_is_usable(&self.cfg.model_path)
    }

    async fn ensure_engine(&self) -> Result<Arc<LlamaEngine>> {
        let mut slot = self.engine.lock().await;
        if let Some(eng) = slot.as_ref() {
            return Ok(eng.clone());
        }

        if !llama_engine::IS_LINKED {
            return Err(LlmError::NotConfigured(
                "llama_local provider: cos was built without --features llama_cpp; \
                 rebuild with the feature enabled to use a local GGUF model"
                    .into(),
            ));
        }

        let cfg = self.cfg.clone();
        // Engine construction may do non-trivial work even at Phase 0.5
        // (backend_init), so push to the blocking pool to be safe.
        let engine = tokio::task::spawn_blocking(move || LlamaEngine::new(cfg))
            .await
            .map_err(|e| LlmError::Internal(format!("spawn_blocking join failed: {e}")))?
            .map_err(|e| LlmError::NotConfigured(format!("llama_cpp engine: {e}")))?;
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

    #[test]
    fn is_configured_handles_existing_file_per_feature() {
        let tmp = std::env::temp_dir().join(format!(
            "cos-llama-prov-{}-{}.gguf",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&tmp, b"placeholder").unwrap();
        let spec = tmp.to_string_lossy().to_string();
        let p = LlamaLocalProvider::new(&spec, &AgentConfig::default());

        // is_configured ANDs file presence with feature-linked. With the
        // feature OFF, even a real file should not flip it on.
        if llama_engine::IS_LINKED {
            assert!(p.is_configured());
        } else {
            assert!(!p.is_configured());
        }
        let _ = std::fs::remove_file(&tmp);
    }

    /// Without the `llama_cpp` feature, asking for a chat must surface
    /// the "not configured" error so the user gets a clear pointer to
    /// rebuild with the feature.
    #[cfg(not(feature = "llama_cpp"))]
    #[tokio::test]
    async fn chat_without_feature_returns_not_configured() {
        let p = LlamaLocalProvider::new("/tmp/anything.gguf", &AgentConfig::default());
        let err = p.chat(req("/tmp/anything.gguf", "hi")).await.unwrap_err();
        match err {
            LlmError::NotConfigured(msg) => {
                assert!(msg.contains("llama_cpp"), "unexpected message: {msg}");
            }
            other => panic!("expected NotConfigured, got {other:?}"),
        }
    }

    /// With the feature on, chat reaches the engine but
    /// `engine.generate()` returns the Phase-0.5 "pending" error.
    #[cfg(feature = "llama_cpp")]
    #[tokio::test]
    async fn chat_with_feature_reaches_engine_and_reports_pending() {
        let tmp = std::env::temp_dir().join(format!(
            "cos-llama-prov-chat-{}-{}.gguf",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&tmp, b"placeholder").unwrap();
        let spec = tmp.to_string_lossy().to_string();
        let p = LlamaLocalProvider::new(&spec, &AgentConfig::default());
        let err = p.chat(req(&spec, "hi")).await.unwrap_err();
        match err {
            LlmError::Internal(msg) => {
                assert!(
                    msg.contains("pending") || msg.contains("Phase"),
                    "unexpected internal msg: {msg}"
                );
            }
            other => panic!("expected Internal pending, got {other:?}"),
        }
        let _ = std::fs::remove_file(&tmp);
    }
}
