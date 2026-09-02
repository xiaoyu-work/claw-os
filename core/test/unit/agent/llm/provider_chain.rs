use super::*;
use crate::agent::llm::attempt_observer::NoopProviderAttemptObserver;
use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
use crate::config::AgentConfig;
use std::sync::Mutex;

fn slot(name: &str, model: &str, responses: Vec<MockResponse>) -> ProviderSlot {
    let provider = Arc::new(MockProvider::new(model, &AgentConfig::default()));
    for response in responses {
        provider.push_response(response);
    }

    ProviderSlot::new(provider, name, model)
}

struct CoolingProvider {
    pool: crate::agent::llm::credential_pool::Pool,
}

impl CoolingProvider {
    fn new() -> Self {
        use crate::agent::llm::credential_pool::{
            FailureClass, Pool, PoolEntry, SelectionStrategy,
        };
        let pool = Pool::from_entries(
            "primary",
            vec![PoolEntry::inline("primary-key")],
            SelectionStrategy::Sticky,
        )
        .unwrap();
        let lease = pool.acquire().unwrap();
        pool.report_failure(&lease, FailureClass::CooldownWorthy);
        Self { pool }
    }
}

#[async_trait]
impl Provider for CoolingProvider {
    fn name(&self) -> &str {
        "cooling-primary"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["p-model".to_string()]
    }

    fn is_configured(&self) -> bool {
        true
    }

    async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
        self.pool.acquire()?;
        unreachable!("a cooling pool must not return a lease")
    }

    async fn chat_stream(
        &self,
        _request: ChatRequest,
    ) -> Result<futures_util::stream::BoxStream<'static, Result<StreamEvent>>> {
        self.pool.acquire()?;
        unreachable!("a cooling pool must not return a lease")
    }
}

#[tokio::test]
async fn falls_back_on_transient_error_and_sticks() {
    let chain = ProviderChain::new_with_observer(
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
        Arc::new(NoopProviderAttemptObserver),
    )
    .unwrap();
    let first = chain.chat(request("p-model")).await.unwrap();
    assert_eq!(text(&first), "fallback answer");
    assert_eq!(chain.effective_provider_name(), "fallback");
    let second = chain.chat(request("p-model")).await.unwrap();
    assert_eq!(text(&second), "still fallback");
}

#[tokio::test]
async fn configured_primary_pool_cooldown_falls_back_and_preserves_retry_semantics() {
    let chain = ProviderChain::new(vec![
        ProviderSlot::new(Arc::new(CoolingProvider::new()), "primary", "p-model"),
        slot(
            "fallback",
            "f-model",
            vec![MockResponse::Text("fallback answer".to_string())],
        ),
    ])
    .unwrap();

    let response = chain.chat(request("p-model")).await.unwrap();

    assert_eq!(text(&response), "fallback answer");
    let state = chain.fallback_state_snapshot();
    assert!(state.degraded);
    assert_eq!(state.switches[0].failure_class, "cooldown-worthy");
}

#[tokio::test]
async fn caller_error_does_not_fallback() {
    let chain = ProviderChain::new_with_observer(
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
        Arc::new(NoopProviderAttemptObserver),
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
    let chain = ProviderChain::new_with_observer(
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
        Arc::new(NoopProviderAttemptObserver),
    )
    .unwrap();
    assert!(chain.chat(request("p-model")).await.is_err());
    assert_eq!(chain.effective_provider_name(), "primary");
    assert!(!chain.fallback_state_snapshot().degraded);
}

#[derive(Default)]
struct RecordingObserver {
    switches: Mutex<Vec<ProviderSwitch>>,
}

impl ProviderAttemptObserver for RecordingObserver {
    fn observe_switch(&self, record: &ProviderSwitch) {
        self.switches.lock().unwrap().push(record.clone());
    }
}

#[tokio::test]
async fn reports_switch_through_injected_observer() {
    let observer = Arc::new(RecordingObserver::default());
    let chain = ProviderChain::new_with_observer(
        vec![
            slot(
                "primary",
                "p-model",
                vec![MockResponse::Error(LlmError::Auth)],
            ),
            slot(
                "fallback",
                "f-model",
                vec![MockResponse::Text("answer".to_string())],
            ),
        ],
        observer.clone(),
    )
    .unwrap();

    chain.chat(request("p-model")).await.unwrap();

    let switches = observer.switches.lock().unwrap();
    assert_eq!(switches.len(), 1);
    assert_eq!(switches[0].from_provider, "primary");
    assert_eq!(switches[0].to_provider, "fallback");
    assert_eq!(switches[0].failure_class, "cooldown-worthy");
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
