use async_trait::async_trait;
use futures_util::stream::{self, BoxStream};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

use super::credential_pool::FailureClass;
use super::{
    error_classifier, ChatRequest, ChatResponse, EngineInfo, LlmError, Provider, Result,
    StreamEvent,
};

const MAX_PROVIDER_SLOTS: usize = 9;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSwitch {
    pub from_provider: String,
    pub from_model: String,
    pub to_provider: String,
    pub to_model: String,
    pub failure_class: String,
    pub reason: String,
    pub switched_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFallbackState {
    pub primary_provider: String,
    pub primary_model: String,
    pub active_provider: String,
    pub active_model: String,
    pub degraded: bool,
    pub switches: Vec<ProviderSwitch>,
}

pub struct ProviderSlot {
    pub provider: Arc<dyn Provider>,
    pub provider_name: String,
    pub model: String,
}

impl ProviderSlot {
    pub fn new(
        provider: Arc<dyn Provider>,
        provider_name: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            provider_name: provider_name.into(),
            model: model.into(),
        }
    }
}

struct ChainState {
    active: usize,
    switches: Vec<ProviderSwitch>,
}

pub struct ProviderChain {
    slots: Vec<ProviderSlot>,
    state: Mutex<ChainState>,
    audit_path: Option<std::path::PathBuf>,
}

impl ProviderChain {
    pub fn new(slots: Vec<ProviderSlot>) -> Result<Self> {
        Self::with_audit_path(slots, Some(crate::paths::agent_audit_log_path()))
    }

    fn with_audit_path(
        slots: Vec<ProviderSlot>,
        audit_path: Option<std::path::PathBuf>,
    ) -> Result<Self> {
        if slots.is_empty() {
            return Err(LlmError::NotConfigured(
                "provider fallback chain is empty".to_string(),
            ));
        }
        if slots.len() > MAX_PROVIDER_SLOTS {
            return Err(LlmError::NotConfigured(format!(
                "provider fallback chain has {} slots; maximum is {MAX_PROVIDER_SLOTS}",
                slots.len()
            )));
        }
        Ok(Self {
            slots,
            state: Mutex::new(ChainState {
                active: 0,
                switches: Vec::new(),
            }),
            audit_path,
        })
    }

    fn active_index(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.active.min(self.slots.len() - 1))
            .unwrap_or(0)
    }

    fn switch(
        &self,
        from: usize,
        to: usize,
        error: &LlmError,
        class: &'static str,
    ) -> ProviderSwitch {
        let reason = super::truncate_for_display(
            &crate::agent::safety::redact::Redactor::default_set().redact(&error.to_string()),
            200,
        );
        let record = ProviderSwitch {
            from_provider: self.slots[from].provider_name.clone(),
            from_model: self.slots[from].model.clone(),
            to_provider: self.slots[to].provider_name.clone(),
            to_model: self.slots[to].model.clone(),
            failure_class: class.to_string(),
            reason,
            switched_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Ok(mut state) = self.state.lock() {
            state.switches.push(record.clone());
        }
        let mut event = serde_json::json!({
            "kind": "provider_fallback",
            "from_provider": record.from_provider,
            "from_model": record.from_model,
            "to_provider": record.to_provider,
            "to_model": record.to_model,
            "failure_class": record.failure_class,
            "reason": record.reason,
            "switched_at": record.switched_at,
        });
        if let Ok(session_id) = std::env::var("COS_SESSION") {
            if !session_id.is_empty() {
                event["session_id"] = serde_json::json!(session_id);
            }
        }
        if let Some(path) = &self.audit_path {
            crate::audit::log_chained_event(path, event);
        }
        record
    }

    fn mark_success(&self, index: usize) {
        if let Ok(mut state) = self.state.lock() {
            state.active = state.active.max(index);
        }
    }

    fn fallback_state_snapshot(&self) -> ProviderFallbackState {
        let (active, switches) = self
            .state
            .lock()
            .map(|state| {
                (
                    state.active.min(self.slots.len() - 1),
                    state.switches.clone(),
                )
            })
            .unwrap_or_else(|_| (0, Vec::new()));
        ProviderFallbackState {
            primary_provider: self.slots[0].provider_name.clone(),
            primary_model: self.slots[0].model.clone(),
            active_provider: self.slots[active].provider_name.clone(),
            active_model: self.slots[active].model.clone(),
            degraded: active > 0,
            switches,
        }
    }
}

#[async_trait]
impl Provider for ProviderChain {
    fn name(&self) -> &str {
        "provider-chain"
    }

    fn supported_models(&self) -> Vec<String> {
        let mut models = self
            .slots
            .iter()
            .flat_map(|slot| slot.provider.supported_models())
            .collect::<Vec<_>>();
        models.sort();
        models.dedup();
        models
    }

    fn is_configured(&self) -> bool {
        self.slots.iter().any(|slot| slot.provider.is_configured())
    }

    fn engine_info(&self) -> Option<EngineInfo> {
        self.slots[self.active_index()].provider.engine_info()
    }

    fn supports_prompt_cache(&self) -> bool {
        self.slots[self.active_index()]
            .provider
            .supports_prompt_cache()
    }

    fn effective_provider_name(&self) -> String {
        self.fallback_state_snapshot().active_provider
    }

    fn effective_model_name(&self, _requested: &str) -> String {
        self.fallback_state_snapshot().active_model
    }

    fn fallback_state(&self) -> Option<ProviderFallbackState> {
        Some(self.fallback_state_snapshot())
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let start = self.active_index();
        let mut request = request;
        for index in start..self.slots.len() {
            request.model = self.slots[index].model.clone();
            match self.slots[index].provider.chat(request.clone()).await {
                Ok(response) => {
                    self.mark_success(index);
                    return Ok(response);
                }
                Err(error) => {
                    let Some(class) = fallback_class(&error) else {
                        return Err(error);
                    };
                    let Some(next) = index.checked_add(1).filter(|next| *next < self.slots.len())
                    else {
                        return Err(error);
                    };
                    self.switch(index, next, &error, class);
                }
            }
        }
        Err(LlmError::Internal(
            "provider fallback chain exhausted without a result".to_string(),
        ))
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let start = self.active_index();
        let mut request = request;
        let mut warnings = Vec::new();
        for index in start..self.slots.len() {
            request.model = self.slots[index].model.clone();
            match self.slots[index]
                .provider
                .chat_stream(request.clone())
                .await
            {
                Ok(provider_stream) => {
                    self.mark_success(index);
                    let prefix =
                        stream::iter(warnings.into_iter().map(|record: ProviderSwitch| {
                            Ok(StreamEvent::Warning {
                                message: format!(
                                    "provider fallback: {} / {} -> {} / {} ({})",
                                    record.from_provider,
                                    record.from_model,
                                    record.to_provider,
                                    record.to_model,
                                    record.failure_class
                                ),
                            })
                        }));
                    return Ok(prefix.chain(provider_stream).boxed());
                }
                Err(error) => {
                    let Some(class) = fallback_class(&error) else {
                        return Err(error);
                    };
                    let Some(next) = index.checked_add(1).filter(|next| *next < self.slots.len())
                    else {
                        return Err(error);
                    };
                    warnings.push(self.switch(index, next, &error, class));
                }
            }
        }
        Err(LlmError::Internal(
            "provider fallback stream chain exhausted without a result".to_string(),
        ))
    }
}

fn fallback_class(error: &LlmError) -> Option<&'static str> {
    match error {
        LlmError::Transport(_) => Some("transient"),
        LlmError::RateLimited { .. } | LlmError::Auth | LlmError::NotConfigured(_) => {
            Some("cooldown-worthy")
        }
        LlmError::Provider { status, message } => {
            match error_classifier::classify(*status, message) {
                FailureClass::CooldownWorthy => Some("cooldown-worthy"),
                FailureClass::Transient => Some("transient"),
                FailureClass::CallerError => None,
            }
        }
        LlmError::Parse(_) | LlmError::UpstreamMalformed(_) | LlmError::Stream(_) => {
            Some("upstream-malformed")
        }
        LlmError::InvalidRequest(_) | LlmError::Internal(_) => None,
    }
}

#[cfg(test)]
mod tests {
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
}
