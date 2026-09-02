//! Observation seam for provider fallback attempts.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};

use super::provider_chain::ProviderSwitch;
use super::{
    ChatRequest, ChatResponse, EngineInfo, LlmError, Provider, Result, StreamEvent, Usage,
};

/// Metadata captured by the caller for provider-attempt records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestMetadata {
    pub session_id: Option<String>,
}

impl RequestMetadata {
    /// Capture process request metadata at an explicit composition boundary.
    pub fn from_process() -> Self {
        Self {
            session_id: std::env::var("COS_SESSION")
                .ok()
                .filter(|session_id| !session_id.is_empty()),
        }
    }
}

/// Receives provider switches without participating in fallback decisions.
///
/// Observation is intentionally infallible: the audit implementation logs
/// append failures as warnings, matching the existing audit semantics without
/// turning an otherwise successful fallback into a request failure.
pub trait ProviderAttemptObserver: Send + Sync {
    fn observe_switch(&self, record: &ProviderSwitch);

    fn observe_start(&self, _record: &ProviderAttemptStart) {}

    fn observe_finish(&self, _record: &ProviderAttemptFinish) {}
}

#[derive(Debug, Default)]
pub struct NoopProviderAttemptObserver;

impl ProviderAttemptObserver for NoopProviderAttemptObserver {
    fn observe_switch(&self, _record: &ProviderSwitch) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderDelivery {
    Buffered,
    Streaming,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAttemptStart {
    pub attempt_id: String,
    pub turn_index: u32,
    pub provider: String,
    pub model: String,
    pub delivery: ProviderDelivery,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderAttemptOutcome {
    Success,
    Error(&'static str),
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct ProviderAttemptFinish {
    pub start: ProviderAttemptStart,
    pub latency_ms: u64,
    pub usage: Usage,
    pub outcome: ProviderAttemptOutcome,
}

pub struct CompositeProviderAttemptObserver {
    observers: Vec<Arc<dyn ProviderAttemptObserver>>,
}

impl CompositeProviderAttemptObserver {
    pub fn new(observers: Vec<Arc<dyn ProviderAttemptObserver>>) -> Self {
        Self { observers }
    }
}

impl ProviderAttemptObserver for CompositeProviderAttemptObserver {
    fn observe_switch(&self, record: &ProviderSwitch) {
        for observer in &self.observers {
            observer.observe_switch(record);
        }
    }

    fn observe_start(&self, record: &ProviderAttemptStart) {
        for observer in &self.observers {
            observer.observe_start(record);
        }
    }

    fn observe_finish(&self, record: &ProviderAttemptFinish) {
        for observer in &self.observers {
            observer.observe_finish(record);
        }
    }
}

pub struct ObservedProvider {
    inner: Arc<dyn Provider>,
    provider: String,
    observer: Arc<dyn ProviderAttemptObserver>,
}

impl ObservedProvider {
    pub fn new(
        inner: Arc<dyn Provider>,
        provider: impl Into<String>,
        observer: Arc<dyn ProviderAttemptObserver>,
    ) -> Self {
        Self {
            inner,
            provider: provider.into(),
            observer,
        }
    }

    fn start(&self, request: &ChatRequest, delivery: ProviderDelivery) -> AttemptSpan {
        let turn_index = request
            .extra
            .get("_cos_turn_index")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        AttemptSpan::new(
            Arc::clone(&self.observer),
            ProviderAttemptStart {
                attempt_id: uuid::Uuid::new_v4().simple().to_string(),
                turn_index,
                provider: self.provider.clone(),
                model: request.model.clone(),
                delivery,
            },
        )
    }
}

struct AttemptSpan {
    observer: Arc<dyn ProviderAttemptObserver>,
    start: ProviderAttemptStart,
    started: Instant,
    finished: bool,
}

impl AttemptSpan {
    fn new(observer: Arc<dyn ProviderAttemptObserver>, start: ProviderAttemptStart) -> Self {
        observer.observe_start(&start);
        Self {
            observer,
            start,
            started: Instant::now(),
            finished: false,
        }
    }

    fn finish(&mut self, outcome: ProviderAttemptOutcome, usage: Usage) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.observer.observe_finish(&ProviderAttemptFinish {
            start: self.start.clone(),
            latency_ms: self.started.elapsed().as_millis() as u64,
            usage,
            outcome,
        });
    }
}

impl Drop for AttemptSpan {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(ProviderAttemptOutcome::Cancelled, Usage::default());
        }
    }
}

#[async_trait]
impl Provider for ObservedProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn supported_models(&self) -> Vec<String> {
        self.inner.supported_models()
    }

    fn is_configured(&self) -> bool {
        self.inner.is_configured()
    }

    fn engine_info(&self) -> Option<EngineInfo> {
        self.inner.engine_info()
    }

    fn supports_prompt_cache(&self) -> bool {
        self.inner.supports_prompt_cache()
    }

    fn effective_provider_name(&self) -> String {
        self.inner.effective_provider_name()
    }

    fn effective_model_name(&self, requested: &str) -> String {
        self.inner.effective_model_name(requested)
    }

    fn fallback_state(&self) -> Option<super::ProviderFallbackState> {
        self.inner.fallback_state()
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let mut span = self.start(&request, ProviderDelivery::Buffered);
        let result = self.inner.chat(request).await;
        match &result {
            Ok(response) => span.finish(ProviderAttemptOutcome::Success, response.usage.clone()),
            Err(error) => span.finish(
                ProviderAttemptOutcome::Error(error_class(error)),
                Usage::default(),
            ),
        }
        result
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let mut span = self.start(&request, ProviderDelivery::Streaming);
        let stream = match self.inner.chat_stream(request).await {
            Ok(stream) => stream,
            Err(error) => {
                span.finish(
                    ProviderAttemptOutcome::Error(error_class(&error)),
                    Usage::default(),
                );
                return Err(error);
            }
        };
        Ok(
            stream::unfold((stream, Some(span)), |(mut inner, mut span)| async move {
                match inner.next().await {
                    Some(item) => {
                        match &item {
                            Ok(StreamEvent::Done { usage, .. }) => {
                                if let Some(span) = span.as_mut() {
                                    span.finish(ProviderAttemptOutcome::Success, usage.clone());
                                }
                                span = None;
                            }
                            Err(error) => {
                                if let Some(span) = span.as_mut() {
                                    span.finish(
                                        ProviderAttemptOutcome::Error(error_class(error)),
                                        Usage::default(),
                                    );
                                }
                                span = None;
                            }
                            _ => {}
                        }
                        Some((item, (inner, span)))
                    }
                    None => {
                        if let Some(span) = span.as_mut() {
                            span.finish(
                                ProviderAttemptOutcome::Error("incomplete_stream"),
                                Usage::default(),
                            );
                        }
                        None
                    }
                }
            })
            .boxed(),
        )
    }
}

fn error_class(error: &LlmError) -> &'static str {
    match error {
        LlmError::NotConfigured(_) => "not_configured",
        LlmError::InvalidRequest(_) => "invalid_request",
        LlmError::Transport(_) => "transport",
        LlmError::Provider { status, .. } if *status == 401 || *status == 403 => "auth",
        LlmError::Provider { status, .. } if *status == 429 => "rate_limited",
        LlmError::Provider { status, .. } if *status >= 500 => "provider_transient",
        LlmError::Provider { .. } => "provider_error",
        LlmError::RateLimited { .. } => "rate_limited",
        LlmError::Auth => "auth",
        LlmError::CredentialStore { .. } => "credential_store",
        LlmError::Infrastructure(_) => "infrastructure",
        LlmError::Parse(_) => "parse",
        LlmError::UpstreamMalformed(_) => "upstream_malformed",
        LlmError::Stream(_) => "stream",
        LlmError::Internal(_) => "internal",
    }
}

#[derive(Debug, Clone)]
pub struct AuditProviderAttemptObserver {
    audit_path: PathBuf,
    metadata: RequestMetadata,
}

impl AuditProviderAttemptObserver {
    pub fn new(audit_path: PathBuf, metadata: RequestMetadata) -> Self {
        Self {
            audit_path,
            metadata,
        }
    }
}

impl ProviderAttemptObserver for AuditProviderAttemptObserver {
    fn observe_switch(&self, record: &ProviderSwitch) {
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
        if let Some(session_id) = &self.metadata.session_id {
            event["session_id"] = serde_json::json!(session_id);
        }
        crate::audit::log_chained_event(&self.audit_path, event);
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/attempt_observer.rs"
    ));
}
