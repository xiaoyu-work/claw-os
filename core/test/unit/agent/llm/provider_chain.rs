use super::*;
use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
use crate::config::AgentConfig;

fn slot(name: &str, model: &str, responses: Vec<MockResponse>) -> ProviderSlot {
    let provider = Arc::new(MockProvider::new(model, &AgentConfig::default()));
    for response in responses {
        provider.push_response(response);
    }
    ProviderSlot::new(provider, name, model)
}

#[tokio::test]
async fn falls_back_on_transient_error_and_sticks() {
    let chain = ProviderChain::with_audit_path(
        vec![
            slot(
                "primary",
                "p-model",
                vec![MockResponse::Error(LlmError::Provider {
                    status: 503,
                    message: "unavailable".to_string(),
                })],
            ),
            slot(
                "fallback",
                "f-model",
                vec![
                    MockResponse::Text("fallback answer".to_string()),
                    MockResponse::Text("still fallback".to_string()),
                ],
            ),
        ],
        None,
    )
    .unwrap();
    let first = chain.chat(request("p-model")).await.unwrap();
    assert_eq!(text(&first), "fallback answer");
    assert_eq!(chain.effective_provider_name(), "fallback");
    let second = chain.chat(request("p-model")).await.unwrap();
    assert_eq!(text(&second), "still fallback");
}

#[tokio::test]
async fn caller_error_does_not_fallback() {
    let chain = ProviderChain::with_audit_path(
        vec![
            slot(
                "primary",
                "p-model",
                vec![MockResponse::Error(LlmError::InvalidRequest(
                    "bad schema".to_string(),
                ))],
            ),
            slot(
                "fallback",
                "f-model",
                vec![MockResponse::Text("must not run".to_string())],
            ),
        ],
        None,
    )
    .unwrap();
    assert!(matches!(
        chain.chat(request("p-model")).await,
        Err(LlmError::InvalidRequest(_))
    ));
    assert_eq!(chain.effective_provider_name(), "primary");
}

#[tokio::test]
async fn exhausted_chain_does_not_pin_failed_fallback() {
    let chain = ProviderChain::with_audit_path(
        vec![
            slot(
                "primary",
                "p-model",
                vec![MockResponse::Error(LlmError::Auth)],
            ),
            slot(
                "fallback",
                "f-model",
                vec![MockResponse::Error(LlmError::RateLimited {
                    retry_after_ms: 1,
                })],
            ),
        ],
        None,
    )
    .unwrap();
    assert!(chain.chat(request("p-model")).await.is_err());
    assert_eq!(chain.effective_provider_name(), "primary");
    assert!(!chain.fallback_state_snapshot().degraded);
}

fn request(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
        messages: vec![super::super::Message::user_text("hello")],
        system: None,
        tools: Vec::new(),
        tool_choice: Default::default(),
        max_tokens: None,
        temperature: None,
        top_p: None,
        stop_sequences: Vec::new(),
        extra: serde_json::Value::Null,
    }
}

fn text(response: &ChatResponse) -> &str {
    response
        .content
        .iter()
        .find_map(|block| match block {
            super::super::ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or_default()
}
