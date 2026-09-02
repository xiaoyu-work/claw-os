//! Observation seam for provider fallback attempts.

use std::path::PathBuf;

use super::provider_chain::ProviderSwitch;

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
}

#[derive(Debug, Default)]
pub struct NoopProviderAttemptObserver;

impl ProviderAttemptObserver for NoopProviderAttemptObserver {
    fn observe_switch(&self, _record: &ProviderSwitch) {}
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
