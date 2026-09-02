use super::*;
use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
use crate::agent::llm::{Message, ToolChoice};
use std::sync::Mutex;

#[derive(Default)]
struct RecordingObserver {
    events: Mutex<Vec<(String, String)>>,
}

impl ProviderAttemptObserver for RecordingObserver {
    fn observe_switch(&self, _record: &ProviderSwitch) {}

    fn observe_start(&self, record: &ProviderAttemptStart) {
        self.events
            .lock()
            .unwrap()
            .push(("pre".to_string(), record.attempt_id.clone()));
    }

    fn observe_finish(&self, record: &ProviderAttemptFinish) {
        self.events
            .lock()
            .unwrap()
            .push(("post".to_string(), record.start.attempt_id.clone()));
    }
}

fn request() -> ChatRequest {
    ChatRequest {
        model: "mock-model".to_string(),
        messages: vec![Message::user_text("hello")],
        system: None,
        tools: Vec::new(),
        tool_choice: ToolChoice::None,
        max_tokens: Some(16),
        temperature: Some(0.0),
        top_p: None,
        stop_sequences: Vec::new(),
        extra: serde_json::json!({"_cos_turn_index": 7}),
    }
}

#[test]
fn no_provider_invocation_emits_no_attempt_events() {
    let observer = RecordingObserver::default();
    assert!(observer.events.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn every_real_provider_invocation_has_one_paired_attempt_id() {
    let config = crate::config::AgentConfig {
        provider: "mock".to_string(),
        model: "mock-model".to_string(),
        ..Default::default()
    };
    let mock = Arc::new(MockProvider::new("mock-model", &config));
    mock.push_response(MockResponse::Error(LlmError::RateLimited {
        retry_after_ms: 1,
    }));
    mock.push_response(MockResponse::Text("ok".to_string()));
    let observer = Arc::new(RecordingObserver::default());
    let provider = Arc::new(ObservedProvider::new(mock, "mock", observer.clone()));
    let retry_provider = Arc::clone(&provider);
    crate::agent::llm::rate_limit::retry_with_backoff(
        crate::agent::llm::rate_limit::RetryPolicy {
            max_attempts: 2,
            base_ms: 1,
            max_ms: 1,
            jitter: false,
        },
        move || {
            let provider = Arc::clone(&retry_provider);
            async move { provider.chat(request()).await }
        },
    )
    .await
    .unwrap();

    let events = observer.events.lock().unwrap().clone();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].0, "pre");
    assert_eq!(events[1], ("post".to_string(), events[0].1.clone()));
    assert_eq!(events[2].0, "pre");
    assert_eq!(events[3], ("post".to_string(), events[2].1.clone()));
    assert_ne!(events[0].1, events[2].1);
}

#[tokio::test]
async fn dropped_stream_emits_one_cancelled_post() {
    let config = crate::config::AgentConfig {
        provider: "mock".to_string(),
        model: "mock-model".to_string(),
        ..Default::default()
    };
    let mock = Arc::new(MockProvider::new("mock-model", &config));
    mock.push_response(MockResponse::Text("ok".to_string()));
    let observer = Arc::new(RecordingObserver::default());
    let provider = ObservedProvider::new(mock, "mock", observer.clone());
    let stream = provider.chat_stream(request()).await.unwrap();
    drop(stream);

    let events = observer.events.lock().unwrap().clone();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].0, "pre");
    assert_eq!(events[1], ("post".to_string(), events[0].1.clone()));
}
