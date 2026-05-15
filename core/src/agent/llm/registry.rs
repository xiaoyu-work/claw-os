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

/// Names of every provider linked into this binary. The OpenAI-compatible
/// provider is registered under multiple aliases (`openai`, `xai`,
/// `deepseek`, `openrouter`, `ollama`) — they all share one impl but get
/// different default base URLs.
pub const REGISTERED: &[&str] = &[
    "mock",
    "llama_local",
    "openai",
    "xai",
    "deepseek",
    "openrouter",
    "ollama",
    "azure",
    "anthropic",
    "bedrock",
    "gemini",
];

/// Construct a provider by name.
///
/// An empty `name` is treated as "not configured" and returns a clear
/// error rather than falling through to the "unknown provider" branch
/// (which would print the misleading message `unknown provider ''`).
/// This is the default state on a fresh install — `AgentConfig::default()`
/// leaves `provider` empty so the OS owner is forced to pick one via
/// `cos agent setup llm apply ...` or the desktop initial-setup AI page.
pub fn build(name: &str, model: &str, agent_cfg: &AgentConfig) -> Result<Arc<dyn Provider>> {
    if name.is_empty() {
        return Err(LlmError::NotConfigured(
            "no LLM provider configured. Run `cos agent setup llm apply \
             --provider <name> --model <id> [--api-key <key>]` or open the \
             desktop initial-setup AI page to pick one."
                .into(),
        ));
    }
    if providers::openai_compat::is_alias(name) {
        return Ok(providers::openai_compat::build_provider(
            name, model, agent_cfg,
        ));
    }
    if providers::anthropic::is_alias(name) {
        return Ok(providers::anthropic::build_provider(model, agent_cfg));
    }
    if providers::bedrock::is_alias(name) {
        return Ok(providers::bedrock::build_provider(model, agent_cfg));
    }
    if providers::gemini::is_alias(name) {
        return Ok(providers::gemini::build_provider(model, agent_cfg));
    }
    match name {
        "mock" => Ok(Arc::new(providers::mock::MockProvider::new(
            model, agent_cfg,
        ))),
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
