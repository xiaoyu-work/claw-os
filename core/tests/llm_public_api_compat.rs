use std::sync::Arc;

use cos::agent::llm::construction::{
    resolve_aws_value, CredentialSource, HttpTransport, ProviderBuildContext,
};
use cos::agent::llm::credential_pool::{Pool, SelectionStrategy};
use cos::agent::llm::provider_chain::{ProviderChain, ProviderSlot};
use cos::agent::llm::providers::{anthropic, bedrock, gemini, openai_compat};
use cos::agent::llm::{Provider, Result};
use cos::config::AgentConfig;

struct ExternalCredentialSource;

impl CredentialSource for ExternalCredentialSource {
    fn load_stored(&self, name: &str) -> std::result::Result<Option<String>, String> {
        Ok((name == "stored").then(|| "stored-value".to_string()))
    }

    fn load_environment(&self, name: &str) -> Option<String> {
        (name == "EXTERNAL_VALUE").then(|| "environment-value".to_string())
    }
}

#[test]
fn legacy_provider_construction_api_remains_available_to_external_crates() {
    let _: fn(
        Option<&str>,
        Option<&str>,
        &str,
        &dyn CredentialSource,
    ) -> Option<String> = resolve_aws_value;
    let source = Arc::new(ExternalCredentialSource);
    let _: ProviderBuildContext =
        ProviderBuildContext::new(source.clone(), HttpTransport::new().unwrap());
    let source_ref: &dyn CredentialSource = source.as_ref();
    assert_eq!(
        resolve_aws_value(
            Some("stored"),
            Some("EXTERNAL_VALUE"),
            "DEFAULT",
            source_ref
        )
        .as_deref(),
        Some("stored-value")
    );

    let _: fn(Vec<ProviderSlot>) -> Result<ProviderChain> = ProviderChain::new;

    let _: fn(anthropic::AnthropicConfig) -> anthropic::AnthropicProvider =
        anthropic::AnthropicProvider::new;
    let _: fn(&str, &AgentConfig) -> Result<anthropic::AnthropicConfig> =
        anthropic::AnthropicConfig::try_from_agent_config;
    let _: fn(&str, &AgentConfig) -> anthropic::AnthropicConfig =
        anthropic::AnthropicConfig::from_agent_config;
    let _: fn(&str, &AgentConfig) -> Result<anthropic::AnthropicProvider> =
        anthropic::AnthropicProvider::try_from_agent_config;
    let _: fn(&str, &AgentConfig) -> anthropic::AnthropicProvider =
        anthropic::AnthropicProvider::from_agent_config;
    let _: fn(&str, &AgentConfig) -> Result<Arc<dyn Provider>> = anthropic::build_provider;

    let _: fn(gemini::GeminiConfig) -> gemini::GeminiProvider = gemini::GeminiProvider::new;
    let _: fn(&str, &AgentConfig) -> Result<gemini::GeminiConfig> =
        gemini::GeminiConfig::try_from_agent_config;
    let _: fn(&str, &AgentConfig) -> gemini::GeminiConfig = gemini::GeminiConfig::from_agent_config;
    let _: fn(&str, &AgentConfig) -> Result<gemini::GeminiProvider> =
        gemini::GeminiProvider::try_from_agent_config;
    let _: fn(&str, &AgentConfig) -> gemini::GeminiProvider =
        gemini::GeminiProvider::from_agent_config;
    let _: fn(&str, &AgentConfig) -> Result<Arc<dyn Provider>> = gemini::build_provider;

    let _: fn(bedrock::BedrockConfig) -> bedrock::BedrockProvider = bedrock::BedrockProvider::new;
    let _: fn(&str, &AgentConfig) -> bedrock::BedrockConfig =
        bedrock::BedrockConfig::from_agent_config;
    let _: fn(&str, &AgentConfig) -> bedrock::BedrockProvider =
        bedrock::BedrockProvider::from_agent_config;
    let _: fn(&str, &AgentConfig) -> Arc<dyn Provider> = bedrock::build_provider;

    let _: fn(openai_compat::OpenAICompatConfig) -> openai_compat::OpenAICompatProvider =
        openai_compat::OpenAICompatProvider::new;
    let _: fn(&str, &str, &AgentConfig) -> Result<openai_compat::OpenAICompatConfig> =
        openai_compat::OpenAICompatConfig::try_from_agent_config;
    let _: fn(&str, &str, &AgentConfig) -> openai_compat::OpenAICompatConfig =
        openai_compat::OpenAICompatConfig::from_agent_config;
    let _: fn(&str, &str, &AgentConfig) -> Result<openai_compat::OpenAICompatProvider> =
        openai_compat::OpenAICompatProvider::try_from_agent_config;
    let _: fn(&str, &str, &AgentConfig) -> openai_compat::OpenAICompatProvider =
        openai_compat::OpenAICompatProvider::from_agent_config;
    let _: fn(&str, &str, &AgentConfig) -> Result<Arc<dyn Provider>> =
        openai_compat::build_provider;
    let _: fn(Option<&str>, Option<&str>) -> Result<Option<String>> =
        openai_compat::resolve_api_key;

    let pool = Pool::from_sources(
        "legacy-compile",
        &[],
        &[],
        &["inline-key"],
        SelectionStrategy::Sticky,
    )
    .unwrap();
    assert_eq!(pool.len(), 1);
    assert!(!Pool::is_declared(&AgentConfig::default()));
    assert!(
        Pool::try_from_agent_config("legacy-compile", &AgentConfig::default())
            .unwrap()
            .is_none()
    );
}
