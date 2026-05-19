//! Shared application state held by the axum router.

use std::sync::Arc;

use crate::config::AgentConfig;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub cfg: AgentConfig,
    pub token: String,
    pub started_at_unix: u64,
}

impl AppState {
    pub fn new(cfg: AgentConfig, token: String) -> Self {
        let started_at_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            inner: Arc::new(AppStateInner {
                cfg,
                token,
                started_at_unix,
            }),
        }
    }
}
