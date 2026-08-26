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
            extra: serde_json::json!({"_cos_initiator": "agent"}),
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/auxiliary.rs"
    ));
}
