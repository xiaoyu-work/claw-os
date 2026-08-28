//! Explicit infrastructure used while constructing LLM providers.
//!
//! Provider wire modules receive resolved credentials and a shared transport;
//! process environment and credential-store access stay at composition
//! boundaries.

use std::sync::Arc;
use std::time::Duration;

use crate::agent::llm::credential_pool::{Pool, PoolEntry, SelectionStrategy};
use crate::agent::llm::{LlmError, Result};

/// Read-only secret source used by provider composition.
///
/// Implementations return owned values so providers never retain a handle to
/// credential persistence or process environment state.
pub trait CredentialSource: Send + Sync {
    fn load_stored(&self, name: &str) -> std::result::Result<Option<String>, String>;
    fn load_environment(&self, name: &str) -> Option<String>;
}

#[derive(Debug, Default)]
pub struct ProcessCredentialSource;

impl CredentialSource for ProcessCredentialSource {
    fn load_stored(&self, name: &str) -> std::result::Result<Option<String>, String> {
        crate::credential::try_load(name, "agent")
    }

    fn load_environment(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// Shared HTTP connection pool and request timeout policy for LLM providers.
#[derive(Clone)]
pub struct HttpTransport {
    client: reqwest::Client,
}

impl HttpTransport {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("cos-agent/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(60))
            .build()
            .map_err(|error| {
                LlmError::Internal(format!("failed to build provider HTTP transport: {error}"))
            })?;
        Ok(Self { client })
    }

    pub fn post(
        &self,
        url: impl reqwest::IntoUrl,
        request_timeout: Duration,
    ) -> reqwest::RequestBuilder {
        with_timeout(self.client.post(url), request_timeout)
    }
}

fn with_timeout(
    request: reqwest::RequestBuilder,
    request_timeout: Duration,
) -> reqwest::RequestBuilder {
    if request_timeout.is_zero() {
        request
    } else {
        request.timeout(request_timeout)
    }
}

/// Narrow provider-construction dependencies shared across a registry build.
#[derive(Clone)]
pub struct ProviderBuildContext {
    credentials: Arc<dyn CredentialSource>,
    transport: HttpTransport,
}

impl ProviderBuildContext {
    pub fn from_process() -> Result<Self> {
        Ok(Self {
            credentials: Arc::new(ProcessCredentialSource),
            transport: HttpTransport::new()?,
        })
    }

    pub fn new(credentials: Arc<dyn CredentialSource>, transport: HttpTransport) -> Self {
        Self {
            credentials,
            transport,
        }
    }

    pub fn credentials(&self) -> &dyn CredentialSource {
        self.credentials.as_ref()
    }

    pub fn transport(&self) -> HttpTransport {
        self.transport.clone()
    }
}

/// Resolved API-key ownership for one provider.
///
/// A declared pool is authoritative; `api_key` is populated only for the
/// legacy single-key path.
pub struct ResolvedApiCredentials {
    pub api_key: Option<String>,
    pub pool: Option<Arc<Pool>>,
}

impl std::fmt::Debug for ResolvedApiCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedApiCredentials")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("pool_len", &self.pool.as_ref().map(|pool| pool.len()))
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ApiCredentialConfig<'a> {
    pub credential_name: Option<&'a str>,
    pub environment_name: Option<&'a str>,
    pub pool_credentials: &'a [String],
    pub pool_environments: &'a [String],
    pub pool_strategy: &'a str,
    pub pool_cooldown: Duration,
}

impl<'a> ApiCredentialConfig<'a> {
    pub fn from_agent_config(cfg: &'a crate::config::AgentConfig) -> Self {
        Self {
            credential_name: cfg.api_key_credential.as_deref(),
            environment_name: cfg.api_key_env.as_deref(),
            pool_credentials: &cfg.api_key_credentials,
            pool_environments: &cfg.api_key_envs,
            pool_strategy: &cfg.pool_strategy,
            pool_cooldown: Duration::from_secs(cfg.pool_cooldown_secs),
        }
    }

    pub fn pool_declared(self) -> bool {
        !self.pool_credentials.is_empty() || !self.pool_environments.is_empty()
    }
}

pub fn resolve_api_credentials(
    pool_name: impl Into<String>,
    cfg: ApiCredentialConfig<'_>,
    source: &dyn CredentialSource,
) -> Result<ResolvedApiCredentials> {
    let pool_name = pool_name.into();
    if cfg.pool_declared() {
        let mut entries = Vec::new();
        for credential in cfg.pool_credentials {
            match source
                .load_stored(credential)
                .map_err(|message| LlmError::CredentialStore {
                    credential: credential.clone(),
                    message,
                })? {
                Some(value) if !value.trim().is_empty() => entries.push(
                    PoolEntry::from_credential(credential.clone(), value.trim().to_string()),
                ),
                Some(_) | None => {}
            }
        }
        for env_name in cfg.pool_environments {
            if let Some(value) = source.load_environment(env_name) {
                let value = value.trim();
                if !value.is_empty() {
                    entries.push(PoolEntry::from_env(env_name.clone(), value.to_string()));
                }
            }
        }
        if entries.is_empty() {
            return Err(LlmError::NotConfigured(format!(
                "credential pool '{pool_name}' was declared but no usable key resolved; \
                 unresolved credential names: {:?}; unresolved environment variables: {:?}. \
                 Store a listed credential with `cos credential store <name> <key> \
                 --namespace agent` or set a listed environment variable",
                cfg.pool_credentials, cfg.pool_environments
            )));
        }
        let strategy = SelectionStrategy::from_str_lossy(cfg.pool_strategy);
        let mut pool = Pool::from_entries(pool_name, entries, strategy).map_err(|error| {
            LlmError::NotConfigured(format!(
                "{error}. Fix the declared pool sources, then run \
                 `cos agent setup text --verify-only`"
            ))
        })?;
        pool.set_cooldown(cfg.pool_cooldown);
        return Ok(ResolvedApiCredentials {
            api_key: None,
            pool: Some(Arc::new(pool)),
        });
    }

    Ok(ResolvedApiCredentials {
        api_key: resolve_single_api_key(
            cfg.credential_name,
            cfg.environment_name,
            source,
        )?,
        pool: None,
    })
}

pub fn resolve_single_api_key(
    credential_name: Option<&str>,
    environment_name: Option<&str>,
    source: &dyn CredentialSource,
) -> Result<Option<String>> {
    if let Some(name) = credential_name {
        match source
            .load_stored(name)
            .map_err(|message| LlmError::CredentialStore {
                credential: name.to_string(),
                message,
            })? {
            Some(value) if !value.trim().is_empty() => {
                return Ok(Some(value.trim().to_string()));
            }
            Some(_) | None => {}
        }
    }
    Ok(environment_name
        .and_then(|name| source.load_environment(name))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty()))
}

/// Resolve a legacy single key from the live process sources.
///
/// Kept as an explicit composition helper for non-chat model setup callers.
pub fn resolve_process_api_key(
    credential_name: Option<&str>,
    environment_name: Option<&str>,
) -> Result<Option<String>> {
    resolve_single_api_key(credential_name, environment_name, &ProcessCredentialSource)
}

/// Resolve one AWS value using stored credential, configured env, then the
/// AWS-standard env name. Store read failures retain the historical Bedrock
/// behavior and make that credential unavailable rather than exposing it.
pub fn resolve_aws_value(
    credential_name: Option<&str>,
    environment_name: Option<&str>,
    default_environment_name: &str,
    source: &dyn CredentialSource,
) -> Option<String> {
    if let Some(name) = credential_name {
        if let Ok(Some(value)) = source.load_stored(name) {
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    source
        .load_environment(environment_name.unwrap_or(default_environment_name))
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/construction.rs"
    ));
}
