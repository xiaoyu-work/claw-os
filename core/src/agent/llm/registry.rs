//! Provider registry — runtime construction of LLM providers by name.
//!
//! Each registered provider exposes a `build(model, &AgentConfig)` factory
//! that returns an `Arc<dyn Provider>`. Adding a new provider is a single
//! `match` arm in `build` (no compile-time magic, no inventory crate, no
//! plugin loader — just a typed registry).
//!
//! Usage:
//! ```ignore
//! let provider = registry::build("mock", "mock-model", &agent_cfg)?;
//! ```

use std::sync::Arc;

use super::providers;
use super::{LlmError, Provider, Result};
use crate::config::AgentConfig;

/// Names of every provider linked into this binary.
pub const REGISTERED: &[&str] = &["mock", "llama_local"];

/// Construct a provider by name.
pub fn build(name: &str, model: &str, agent_cfg: &AgentConfig) -> Result<Arc<dyn Provider>> {
    match name {
        "mock" => Ok(Arc::new(providers::mock::MockProvider::new(model, agent_cfg))),
        "llama_local" => Ok(Arc::new(providers::llama_local::LlamaLocalProvider::new(
            model, agent_cfg,
        ))),
        other => Err(LlmError::NotConfigured(format!(
            "unknown provider '{other}'. registered: {REGISTERED:?}"
        ))),
    }
}

/// Whether a provider name is recognised (linked into this binary).
pub fn is_registered(name: &str) -> bool {
    REGISTERED.contains(&name)
}
