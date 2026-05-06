//! Mock LLM provider — for testing the runtime without making real API calls.
//!
//! Default behaviour: echoes the last user message back, prefixed with
//! `[mock] `. Optional scripted responses can be queued for tests that need
//! the loop to perform tool calls or multi-turn flows.

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use std::sync::Mutex;

use crate::agent::llm::{
    ChatRequest, ChatResponse, ContentBlock, FinishReason, LlmError, Provider, Result, Role,
    StreamEvent, ToolCall, Usage,
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
}

impl MockProvider {
    pub fn new(model: &str, _cfg: &AgentConfig) -> Self {
        Self {
            model: model.to_string(),
            script: Mutex::new(Vec::new()),
            cache_capable: std::sync::atomic::AtomicBool::new(false),
            last_request: Mutex::new(None),
        }
    }

    /// Queue a scripted response. Tests use this to drive specific loop paths.
    pub fn push_response(&self, response: MockResponse) {
        self.script.lock().unwrap().push(response);
    }

    /// Override `Provider::supports_prompt_cache` for cache-marker tests.
    pub fn set_supports_prompt_cache(&self, enabled: bool) {
        self.cache_capable
            .store(enabled, std::sync::atomic::Ordering::SeqCst);
    }

    /// Snapshot of the most recent `ChatRequest` the mock saw, or `None`
    /// if `chat`/`chat_stream` has not been called yet.
    pub fn last_request(&self) -> Option<ChatRequest> {
        self.last_request.lock().unwrap().clone()
    }

    fn next_scripted(&self) -> Option<MockResponse> {
        let mut q = self.script.lock().unwrap();
        if q.is_empty() {
            None
        } else {
            Some(q.remove(0))
        }
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
        self.cache_capable
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        *self.last_request.lock().unwrap() = Some(request.clone());

        let response_kind = self
            .next_scripted()
            .unwrap_or_else(|| MockResponse::Text(format!("[mock] {}", extract_last_user_text(&request))));

        match response_kind {
            MockResponse::Text(text) => Ok(ChatResponse {
                model: self.model.clone(),
                content: vec![ContentBlock::Text { text }],
                tool_calls: Vec::new(),
                finish_reason: FinishReason::Stop,
                usage: Usage::default(),
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
                    usage: Usage::default(),
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
    use super::*;
    use crate::agent::llm::Message;

    fn make() -> MockProvider {
        MockProvider::new("mock-model", &AgentConfig::default())
    }

    fn req(text: &str) -> ChatRequest {
        ChatRequest {
            model: "mock-model".into(),
            messages: vec![Message::user_text(text)],
            system: None,
            tools: vec![],
            tool_choice: Default::default(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop_sequences: vec![],
            extra: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn echoes_last_user_message_by_default() {
        let p = make();
        let resp = p.chat(req("hello world")).await.unwrap();
        let text = match &resp.content[0] {
            ContentBlock::Text { text } => text.clone(),
            _ => panic!("expected text block"),
        };
        assert!(text.contains("hello world"));
        assert_eq!(resp.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn scripted_tool_use_then_text() {
        let p = make();
        p.push_response(MockResponse::ToolUse(vec![ToolCall {
            id: "call_1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "hi"}),
        }]));
        p.push_response(MockResponse::Text("done".into()));

        let r1 = p.chat(req("call a tool")).await.unwrap();
        assert_eq!(r1.finish_reason, FinishReason::ToolUse);
        assert_eq!(r1.tool_calls.len(), 1);

        let r2 = p.chat(req("after tool")).await.unwrap();
        assert_eq!(r2.finish_reason, FinishReason::Stop);
    }
}
