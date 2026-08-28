//! Provider registry — runtime construction of LLM providers by name.
//!
//! Provider wire modules receive resolved configuration and infrastructure
//! from this composition boundary. Adding a provider remains a single typed
//! branch with no plugin loader or compile-time registry magic.
//!
//! Usage:
//! ```ignore
//! let provider = registry::build("mock", "mock-model", &agent_cfg)?;
//! ```

use std::sync::Arc;
use std::time::Duration;

use super::construction::{
    resolve_api_credentials, resolve_aws_value, ApiCredentialConfig, ProviderBuildContext,
    ResolvedApiCredentials,
};
use super::providers;
use super::{LlmError, Provider, Result};
use crate::config::AgentConfig;

/// Names of every provider linked into this binary. The OpenAI-compatible
/// provider is registered under multiple aliases (`openai`, `xai`,
/// `deepseek`, `openrouter`, `ollama`, `copilot`) — they all share one
/// impl but get different default base URLs and per-alias auth/header
/// handling.
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
    "copilot",
];

/// Construct a provider by name.
///
/// An empty `name` is treated as "not configured" and returns a clear
/// error rather than falling through to the "unknown provider" branch
/// (which would print the misleading message `unknown provider ''`).
/// This is the default state on a fresh install — `AgentConfig::default()`
/// leaves `provider` empty so the OS owner is forced to pick one via
/// `cos agent setup text apply ...` or the desktop initial-setup AI page.
pub fn build(name: &str, model: &str, agent_cfg: &AgentConfig) -> Result<Arc<dyn Provider>> {
    let context = ProviderBuildContext::from_process()?;
    build_with_context(name, model, agent_cfg, &context)
}

/// Construct a provider from caller-owned infrastructure.
pub fn build_with_context(
    name: &str,
    model: &str,
    agent_cfg: &AgentConfig,
    context: &ProviderBuildContext,
) -> Result<Arc<dyn Provider>> {
    if name.is_empty() {
        return Err(LlmError::NotConfigured(
            "no text-model provider configured. Run `cos agent setup text apply \
             --provider <name> --model <id> [--api-key <key>]` or open the \
             desktop initial-setup AI page to pick one."
                .into(),
        ));
    }
    if providers::openai_compat::is_alias(name) {
        let config = openai_config(name, model, agent_cfg, context)?;
        return Ok(Arc::new(
            providers::openai_compat::OpenAICompatProvider::new(config, context.transport()),
        ));
    }
    if providers::anthropic::is_alias(name) {
        let config = anthropic_config(model, agent_cfg, context)?;
        return Ok(Arc::new(providers::anthropic::AnthropicProvider::new(
            config,
            context.transport(),
        )));
    }
    if providers::bedrock::is_alias(name) {
        let config = bedrock_config(model, agent_cfg, context);
        return Ok(Arc::new(providers::bedrock::BedrockProvider::new(
            config,
            context.transport(),
        )));
    }
    if providers::gemini::is_alias(name) {
        let config = gemini_config(model, agent_cfg, context)?;
        return Ok(Arc::new(providers::gemini::GeminiProvider::new(
            config,
            context.transport(),
        )));
    }
    match name {
        "mock" => {
            Ok(Arc::new(providers::mock::MockProvider::new(model, agent_cfg)) as Arc<dyn Provider>)
        }
        "llama_local" => Ok(Arc::new(providers::llama_local::LlamaLocalProvider::new(
            model, agent_cfg,
        )) as Arc<dyn Provider>),
        other => {
            // LOW: a typo in `agent.provider` is a high-blast-radius
            // misconfiguration. Surface it through both the error
            // path (callers handle) AND a `tracing::warn` so the
            // failure shows up in logs even when the caller silently
            // swallows the error (e.g. config-reload paths).
            tracing::warn!(
                provider = other,
                registered = ?REGISTERED,
                "registry: unknown provider alias; falling back is not possible"
            );
            Err(LlmError::NotConfigured(format!(
                "unknown provider '{other}'. registered: {REGISTERED:?}"
            )))
        }
    }
}

pub(crate) fn openai_config(
    alias: &str,
    model: &str,
    agent: &AgentConfig,
    context: &ProviderBuildContext,
) -> Result<providers::openai_compat::OpenAICompatConfig> {
    let base_url = agent
        .base_url
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| providers::openai_compat::default_base_url_for(alias).to_string())
        .trim_end_matches('/')
        .to_string();
    let ResolvedApiCredentials { api_key, pool } =
        resolve_api_credentials(
            format!("provider:{alias}"),
            ApiCredentialConfig::from_agent_config(agent),
            context.credentials(),
        )?;
    Ok(providers::openai_compat::OpenAICompatConfig {
        alias: alias.to_string(),
        base_url,
        api_key,
        model: model.to_string(),
        extra_headers: agent.extra_headers.clone(),
        request_timeout: request_timeout(agent),
        pool,
    })
}

pub(crate) fn anthropic_config(
    model: &str,
    agent: &AgentConfig,
    context: &ProviderBuildContext,
) -> Result<providers::anthropic::AnthropicConfig> {
    let base_url = agent
        .base_url
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| providers::anthropic::default_base_url().to_string())
        .trim_end_matches('/')
        .to_string();
    let ResolvedApiCredentials { api_key, pool } =
        resolve_api_credentials(
            "provider:anthropic",
            ApiCredentialConfig::from_agent_config(agent),
            context.credentials(),
        )?;
    Ok(providers::anthropic::AnthropicConfig {
        base_url,
        api_key,
        model: model.to_string(),
        extra_headers: agent.extra_headers.clone(),
        request_timeout: request_timeout(agent),
        pool,
    })
}

pub(crate) fn gemini_config(
    model: &str,
    agent: &AgentConfig,
    context: &ProviderBuildContext,
) -> Result<providers::gemini::GeminiConfig> {
    let base_url = agent
        .base_url
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| providers::gemini::default_base_url().to_string())
        .trim_end_matches('/')
        .to_string();
    let ResolvedApiCredentials { api_key, pool } =
        resolve_api_credentials(
            "provider:gemini",
            ApiCredentialConfig::from_agent_config(agent),
            context.credentials(),
        )?;
    Ok(providers::gemini::GeminiConfig {
        base_url,
        api_key,
        model: model.to_string(),
        extra_headers: agent.extra_headers.clone(),
        request_timeout: request_timeout(agent),
        pool,
    })
}

pub(crate) fn bedrock_config(
    model: &str,
    agent: &AgentConfig,
    context: &ProviderBuildContext,
) -> providers::bedrock::BedrockConfig {
    use providers::bedrock::{
        DEFAULT_ACCESS_KEY_ENV, DEFAULT_REGION, DEFAULT_SECRET_KEY_ENV, DEFAULT_SESSION_TOKEN_ENV,
    };

    let credentials = resolve_aws_value(
        agent.aws_access_key_credential.as_deref(),
        agent.aws_access_key_env.as_deref(),
        DEFAULT_ACCESS_KEY_ENV,
        context.credentials(),
    )
    .zip(resolve_aws_value(
        agent.aws_secret_key_credential.as_deref(),
        agent.aws_secret_key_env.as_deref(),
        DEFAULT_SECRET_KEY_ENV,
        context.credentials(),
    ))
    .map(|(access_key, secret_key)| {
        let credentials = crate::agent::llm::sigv4::AwsCredentials::new(access_key, secret_key);
        match resolve_aws_value(
            agent.aws_session_token_credential.as_deref(),
            agent.aws_session_token_env.as_deref(),
            DEFAULT_SESSION_TOKEN_ENV,
            context.credentials(),
        ) {
            Some(token) => credentials.with_session_token(token),
            None => credentials,
        }
    });

    providers::bedrock::BedrockConfig {
        region: agent
            .aws_region
            .clone()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_REGION.to_string()),
        base_url: agent.base_url.clone().filter(|value| !value.is_empty()),
        model: model.to_string(),
        credentials,
        extra_headers: agent.extra_headers.clone(),
        request_timeout: request_timeout(agent),
    }
}

fn request_timeout(agent: &AgentConfig) -> Duration {
    Duration::from_secs(agent.request_timeout)
}

/// Whether a provider name is recognised (linked into this binary).
pub fn is_registered(name: &str) -> bool {
    REGISTERED.contains(&name)
}
