//! Mock LLM provider — for testing the runtime without making real API calls.
//!
//! Default behaviour: echoes the last user message back, prefixed with
//! `[mock] `. Optional scripted responses can be queued for tests that need
//! the loop to perform tool calls or multi-turn flows.

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use std::sync::Mutex;

use crate::agent::llm::{
    ChatRequest, ChatResponse, ContentBlock, FinishReason, LlmError, Provider,
    ProviderInfrastructureError, Result, Role, StreamEvent, ToolCall, Usage,
};
use crate::config::AgentConfig;

/// What the mock should return for the next call.
#[derive(Debug)]
pub enum MockResponse {
    /// Plain text reply, terminates the loop with `FinishReason::Stop`.
    Text(String),
    /// One or more tool calls; loop will dispatch tools and call the mock again.
    ToolUse(Vec<ToolCall>),
    /// Force a synthetic error from `chat()`. Used by tests that
    /// exercise retry / error-path behaviour. The error is consumed
    /// from the script queue like any other response.
    Error(crate::agent::llm::LlmError),
}

pub struct MockProvider {
    model: String,
    /// Scripted queue of responses (popped from the front). When empty, the
    /// mock falls back to echoing the last user message.
    script: Mutex<Vec<MockResponse>>,
    /// Whether `Provider::supports_prompt_cache()` returns true. Default
    /// false; tests for cache marker plumbing flip this on.
    cache_capable: std::sync::atomic::AtomicBool,
    /// Records the most recent `ChatRequest` the mock was called with.
    /// Tests inspect this to assert what reached the provider (e.g. that
    /// the runtime attached prompt-cache markers).
    last_request: Mutex<Option<ChatRequest>>,
    /// Usage to attach to every response. Defaults to all-zeroes;
    /// tests that exercise token-usage plumbing override it.
    usage: Mutex<Usage>,
}

impl MockProvider {
    pub fn new(model: &str, _cfg: &AgentConfig) -> Self {
        Self {
            model: model.to_string(),
            script: Mutex::new(Vec::new()),
            cache_capable: std::sync::atomic::AtomicBool::new(false),
            last_request: Mutex::new(None),
            usage: Mutex::new(Usage::default()),
        }
    }

    /// Queue a scripted response. Tests use this to drive specific loop paths.
    pub fn push_response(&self, response: MockResponse) {
        if let Ok(mut script) = self.script.lock() {
            script.push(response);
        } else {
            tracing::error!("mock provider script state is poisoned");
        }
    }

    /// Override `Provider::supports_prompt_cache` for cache-marker tests.
    pub fn set_supports_prompt_cache(&self, enabled: bool) {
        self.cache_capable
            .store(enabled, std::sync::atomic::Ordering::SeqCst);
    }

    /// Override the [`Usage`] returned with every scripted response.
    /// Used by tests that exercise token-usage plumbing through the
    /// runtime / hooks.
    pub fn set_usage(&self, usage: Usage) {
        if let Ok(mut current) = self.usage.lock() {
            *current = usage;
        } else {
            tracing::error!("mock provider usage state is poisoned");
        }
    }

    /// Snapshot of the most recent `ChatRequest` the mock saw, or `None`
    /// if `chat`/`chat_stream` has not been called yet.
    pub fn last_request(&self) -> Option<ChatRequest> {
        self.last_request
            .lock()
            .map(|request| request.clone())
            .unwrap_or_else(|_| {
                tracing::error!("mock provider request state is poisoned");
                None
            })
    }

    fn next_scripted(&self) -> Result<Option<MockResponse>> {
        let mut q = self.script.lock().map_err(|_| {
            LlmError::from(ProviderInfrastructureError::StatePoisoned {
                component: "mock.script",
            })
        })?;
        if q.is_empty() {
            Ok(None)
        } else {
            Ok(Some(q.remove(0)))
        }
    }

    fn current_usage(&self) -> Result<Usage> {
        self.usage.lock().map(|usage| usage.clone()).map_err(|_| {
            LlmError::from(ProviderInfrastructureError::StatePoisoned {
                component: "mock.usage",
            })
        })
    }
}

fn extract_last_user_text(req: &ChatRequest) -> String {
    req.messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .and_then(|m| {
            m.content.iter().rev().find_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .unwrap_or_else(|| "(no user message)".into())
}

#[async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["mock-model".into(), "echo".into()]
    }

    fn is_configured(&self) -> bool {
        true
    }

    fn supports_prompt_cache(&self) -> bool {
        self.cache_capable.load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        *self.last_request.lock().map_err(|_| {
            LlmError::from(ProviderInfrastructureError::StatePoisoned {
                component: "mock.last_request",
            })
        })? = Some(request.clone());

        let response_kind = self.next_scripted()?.unwrap_or_else(|| {
            MockResponse::Text(format!("[mock] {}", extract_last_user_text(&request)))
        });

        match response_kind {
            MockResponse::Text(text) => Ok(ChatResponse {
                model: self.model.clone(),
                content: vec![ContentBlock::Text { text }],
                tool_calls: Vec::new(),
                finish_reason: FinishReason::Stop,
                usage: self.current_usage()?,
            }),
            MockResponse::ToolUse(calls) => {
                let blocks = calls
                    .iter()
                    .map(|c| ContentBlock::ToolUse {
                        id: c.id.clone(),
                        name: c.name.clone(),
                        input: c.input.clone(),
                    })
                    .collect();
                Ok(ChatResponse {
                    model: self.model.clone(),
                    content: blocks,
                    tool_calls: calls,
                    finish_reason: FinishReason::ToolUse,
                    usage: self.current_usage()?,
                })
            }
            MockResponse::Error(err) => Err(err),
        }
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/providers/mock.rs"
    ));
}
