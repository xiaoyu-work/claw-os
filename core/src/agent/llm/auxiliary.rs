//! Auxiliary LLM client — route lightweight subtasks to a cheap model.
//!
//! The agent often makes throwaway LLM calls that don't need the
//! flagship model: title generation, log summarisation, query
//! rewriting, classification, "is this a yes/no" parsing, etc.
//! Sending those through the user's primary (and possibly expensive)
//! model wastes tokens and adds latency.
//!
//! This module provides a typed handle to a *secondary* provider
//! that the runtime can hand off such subtasks to. Configuration
//! lives in [`crate::config::AgentConfig::auxiliary`]; if absent,
//! the runtime falls back to the primary provider so callers can
//! still proceed.
//!
//! ## Why a wrapper instead of just calling `registry::build`
//!
//! Three reasons:
//!
//!   1. Single source of truth for the "is auxiliary configured?"
//!      check — callers don't need to peek into `AgentConfig`.
//!   2. Hard cap on `max_tokens` for auxiliary calls (default
//!      1024) — these subtasks are *meant* to be short, and capping
//!      at construction time prevents an accidental flagship-sized
//!      request from sneaking through.
//!   3. Centralised typed entry point (`AuxiliaryClient::ask`) so
//!      we can later add caching, rate-limiting, or fallbacks
//!      without touching every caller.
//!
//! ## What `ask` does
//!
//! Builds a minimal [`ChatRequest`] (single user message, no tools,
//! caller-supplied system prompt, capped `max_tokens`), invokes
//! the wrapped provider's `chat`, and returns the assistant's
//! plain-text content. Tool-use blocks in the response are ignored
//! — auxiliary flows are intentionally text-only. Error paths
//! propagate verbatim so callers can decide whether to fall back.

use std::sync::Arc;

use super::types::{ChatRequest, ContentBlock, Message, Role};
use super::{LlmError, Provider, Result};

const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Configuration for [`AuxiliaryClient`].
#[derive(Debug, Clone)]
pub struct AuxiliaryConfig {
    pub provider: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
}

impl AuxiliaryConfig {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            max_tokens: DEFAULT_MAX_TOKENS,
            temperature: None,
        }
    }

    pub fn with_max_tokens(mut self, max: u32) -> Self {
        self.max_tokens = max;
        self
    }

    pub fn with_temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }
}

/// Handle to a configured auxiliary provider.
#[derive(Clone)]
pub struct AuxiliaryClient {
    inner: Arc<dyn Provider>,
    config: AuxiliaryConfig,
}

impl std::fmt::Debug for AuxiliaryClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuxiliaryClient")
            .field("provider_name", &self.inner.name())
            .field("config", &self.config)
            .finish()
    }
}

impl AuxiliaryClient {
    /// Wrap an existing provider with the given configuration.
    pub fn new(provider: Arc<dyn Provider>, config: AuxiliaryConfig) -> Self {
        Self {
            inner: provider,
            config,
        }
    }

    pub fn provider_name(&self) -> &str {
        self.inner.name()
    }

    pub fn config(&self) -> &AuxiliaryConfig {
        &self.config
    }

    /// Run a single-shot text completion. `system` is optional;
    /// `user` is required (non-empty). Tool-use blocks in the
    /// response are dropped — auxiliary calls are text-only.
    pub async fn ask(&self, system: Option<&str>, user: &str) -> Result<String> {
        if user.trim().is_empty() {
            return Err(LlmError::InvalidRequest(
                "auxiliary ask: user message must be non-empty".to_string(),
            ));
        }

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: user.to_string(),
                }],
            }],
            system: system.map(|s| s.to_string()),
            tools: Vec::new(),
            tool_choice: super::types::ToolChoice::Auto,
            max_tokens: Some(self.config.max_tokens),
            temperature: self.config.temperature,
            top_p: None,
            stop_sequences: Vec::new(),
            extra: serde_json::Value::Null,
        };

        let response = self.inner.chat(request).await?;
        let mut buf = String::new();
        for block in response.content {
            if let ContentBlock::Text { text } = block {
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(&text);
            }
        }
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    use crate::config::AgentConfig;

    fn mock_provider(model: &str) -> Arc<dyn Provider> {
        let cfg = AgentConfig::default();
        Arc::new(MockProvider::new(model, &cfg))
    }

    #[test]
    fn config_defaults_max_tokens() {
        let c = AuxiliaryConfig::new("mock", "m");
        assert_eq!(c.max_tokens, DEFAULT_MAX_TOKENS);
        assert!(c.temperature.is_none());
    }

    #[test]
    fn config_builder_chains() {
        let c = AuxiliaryConfig::new("mock", "m")
            .with_max_tokens(64)
            .with_temperature(0.2);
        assert_eq!(c.max_tokens, 64);
        assert_eq!(c.temperature, Some(0.2));
    }

    #[tokio::test]
    async fn ask_returns_provider_text() {
        let provider = mock_provider("echo-model");
        let client = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "echo-model"));
        let out = client.ask(None, "hello world").await.unwrap();
        assert!(out.contains("hello world"), "got: {out}");
    }

    #[tokio::test]
    async fn ask_rejects_empty_user_message() {
        let provider = mock_provider("m");
        let client = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "m"));
        let err = client.ask(None, "   ").await.unwrap_err();
        match err {
            LlmError::InvalidRequest(m) => assert!(m.contains("non-empty")),
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn provider_name_passes_through() {
        let provider = mock_provider("m");
        let client = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "m"));
        assert_eq!(client.provider_name(), "mock");
    }

    #[tokio::test]
    async fn ask_uses_scripted_response_when_present() {
        let cfg = AgentConfig::default();
        let provider = MockProvider::new("m", &cfg);
        provider.push_response(MockResponse::Text("ok".to_string()));
        let provider: Arc<dyn Provider> = Arc::new(provider);
        let client = AuxiliaryClient::new(provider, AuxiliaryConfig::new("mock", "m"));
        let out = client.ask(Some("sys"), "user msg").await.unwrap();
        assert_eq!(out, "ok");
    }

    #[tokio::test]
    async fn ask_concatenates_multiple_text_blocks_in_response() {
        // We can't directly script a multi-block text response via
        // the mock, but we can verify the join behaviour by going
        // through a custom provider that yields multiple text
        // blocks.
        use crate::agent::llm::types::{ChatResponse, FinishReason, StreamEvent, Usage};
        use async_trait::async_trait;
        use futures_util::stream::BoxStream;

        struct MultiBlock;
        #[async_trait]
        impl Provider for MultiBlock {
            fn name(&self) -> &str {
                "multi"
            }
            fn supported_models(&self) -> Vec<String> {
                vec![]
            }
            fn is_configured(&self) -> bool {
                true
            }
            async fn chat(&self, _: ChatRequest) -> Result<ChatResponse> {
                Ok(ChatResponse {
                    model: "x".to_string(),
                    content: vec![
                        ContentBlock::Text {
                            text: "first".to_string(),
                        },
                        ContentBlock::ToolUse {
                            id: "id".to_string(),
                            name: "ignored".to_string(),
                            input: serde_json::json!({}),
                        },
                        ContentBlock::Text {
                            text: "second".to_string(),
                        },
                    ],
                    tool_calls: Vec::new(),
                    finish_reason: FinishReason::Stop,
                    usage: Usage::default(),
                })
            }
            async fn chat_stream(
                &self,
                _: ChatRequest,
            ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
                Err(LlmError::Internal("n/a".to_string()))
            }
        }
        let provider: Arc<dyn Provider> = Arc::new(MultiBlock);
        let client = AuxiliaryClient::new(provider, AuxiliaryConfig::new("multi", "m"));
        let out = client.ask(None, "anything").await.unwrap();
        assert_eq!(out, "first\nsecond");
    }

    #[tokio::test]
    async fn config_max_tokens_caps_request() {
        use crate::agent::llm::types::{ChatResponse, FinishReason, StreamEvent, Usage};
        use async_trait::async_trait;
        use futures_util::stream::BoxStream;
        use std::sync::Mutex;

        struct Capture {
            seen: Mutex<Option<u32>>,
        }
        #[async_trait]
        impl Provider for Capture {
            fn name(&self) -> &str {
                "capture"
            }
            fn supported_models(&self) -> Vec<String> {
                vec![]
            }
            fn is_configured(&self) -> bool {
                true
            }
            async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
                *self.seen.lock().unwrap() = request.max_tokens;
                Ok(ChatResponse {
                    model: request.model,
                    content: vec![ContentBlock::Text {
                        text: String::new(),
                    }],
                    tool_calls: Vec::new(),
                    finish_reason: FinishReason::Stop,
                    usage: Usage::default(),
                })
            }
            async fn chat_stream(
                &self,
                _: ChatRequest,
            ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
                Err(LlmError::Internal("n/a".to_string()))
            }
        }
        let captor = Arc::new(Capture {
            seen: Mutex::new(None),
        });
        let provider: Arc<dyn Provider> = captor.clone();
        let client = AuxiliaryClient::new(
            provider,
            AuxiliaryConfig::new("capture", "m").with_max_tokens(42),
        );
        let _ = client.ask(None, "hi").await.unwrap();
        assert_eq!(*captor.seen.lock().unwrap(), Some(42));
    }
}
