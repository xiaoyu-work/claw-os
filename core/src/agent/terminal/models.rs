use std::sync::Arc;

use crate::config::AgentConfig;

pub(super) struct ModelCatalog {
    pub models: Vec<String>,
    pub warning: Option<String>,
}

pub(super) async fn catalog(
    config: Arc<AgentConfig>,
    ready: bool,
) -> Result<ModelCatalog, String> {
    let mut models = static_catalog(&config);
    let mut warning = None;
    if config.provider == "copilot" && ready {
        match copilot_models(config.clone()).await {
            Ok(discovered) => models.extend(discovered),
            Err(error) => warning = Some(error),
        }
    }
    Ok(ModelCatalog {
        models: validate_catalog(models, &config.model)?,
        warning,
    })
}

async fn copilot_models(config: Arc<AgentConfig>) -> Result<Vec<String>, String> {
    use crate::agent::llm::construction::{
        resolve_api_credentials, ApiCredentialConfig, ProcessCredentialSource,
    };
    use crate::agent::llm::providers::copilot_auth;

    let credentials_config = config;
    let credentials = tokio::task::spawn_blocking(move || {
        resolve_api_credentials(
            "provider:copilot",
            ApiCredentialConfig::from_agent_config(&credentials_config),
            &ProcessCredentialSource,
        )
    })
    .await
    .map_err(|_| "Claw model catalogue credential resolution failed".to_string())?
    .map_err(|_| "Configured Claw model catalogue credentials are unavailable".to_string())?;
    let credential = match credentials.pool {
        Some(pool) => pool
            .acquire()
            .map_err(|_| "No configured Claw catalogue credential is available".to_string())?
            .value()
            .to_string(),
        None => credentials.api_key.ok_or_else(|| {
            "The configured Copilot provider has no catalogue credential".to_string()
        })?,
    };
    let token = copilot_auth::ensure_copilot_token(&credential)
        .await
        .map_err(|_| "Claw Copilot catalogue authentication failed".to_string())?;
    let catalogue = copilot_auth::ensure_copilot_models(&token)
        .await
        .map_err(|_| "The configured Copilot model catalogue is unavailable".to_string())?;
    Ok(catalogue
        .iter()
        .filter(|model| model.is_selectable_chat_model())
        .map(|model| model.id.clone())
        .collect())
}

fn static_catalog(config: &AgentConfig) -> Vec<String> {
    let mut models = Vec::new();
    if !config.model.is_empty() {
        models.push(config.model.clone());
    }
    if uses_native_catalogue_endpoint(config) {
        models.extend(
            crate::agent::llm::metadata::list_for_provider(&config.provider)
                .into_iter()
                .map(|model| model.name.to_string()),
        );
    }
    models
}

fn uses_native_catalogue_endpoint(config: &AgentConfig) -> bool {
    let Some(url) = config.base_url.as_deref().filter(|url| !url.is_empty()) else {
        return true;
    };
    use crate::agent::llm::providers;
    let native = if providers::openai_compat::is_alias(&config.provider) {
        Some(providers::openai_compat::default_base_url_for(
            &config.provider,
        ))
    } else if providers::anthropic::is_alias(&config.provider) {
        Some(providers::anthropic::default_base_url())
    } else if providers::gemini::is_alias(&config.provider) {
        Some(providers::gemini::default_base_url())
    } else {
        None
    };
    native.is_some_and(|native| url.trim_end_matches('/') == native.trim_end_matches('/'))
}

fn validate_catalog(mut models: Vec<String>, primary: &str) -> Result<Vec<String>, String> {
    if models.len() > 1_000 {
        return Err("Claw model catalogue exceeds its terminal bound".into());
    }
    let redactor = crate::agent::safety::redact::Redactor::default_set();
    for model in &models {
        crate::agent::service::validate_requested_model(Some(model))?;
        if redactor.redact(model) != *model {
            return Err("Claw model catalogue contains a sensitive identifier".into());
        }
    }
    models.sort();
    models.dedup();
    if let Some(index) = models.iter().position(|model| model == primary) {
        let primary = models.remove(index);
        models.insert(0, primary);
    }
    Ok(models)
}
