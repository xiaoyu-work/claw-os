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
