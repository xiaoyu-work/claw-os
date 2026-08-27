//! OpenAI-compatible chat-completions provider.
//!
//! One implementation, many backends. Anything that speaks the
//! `POST /v1/chat/completions` shape — OpenAI, xAI, DeepSeek,
//! OpenRouter, Together, Groq, Anyscale, Fireworks, self-hosted vLLM /
//! TGI / LMStudio, Ollama (`/v1` adapter) — all reachable by changing
//! [`AgentConfig::base_url`] and the credential.
//!
//! Configuration model (read at construction by `registry::build`):
//!
//!   - `base_url`              `https://api.openai.com/v1` (provider default)
//!   - `api_key_credential`    name of the cred in `cos credential` namespace `agent`
//!   - `api_key_env`           env var fallback (e.g. `OPENAI_API_KEY`)
//!   - `extra_headers`         arbitrary headers (OpenRouter `HTTP-Referer` etc.)
//!   - `request_timeout`       per-request timeout, seconds
//!
//! Tool-calling: `tools` are forwarded as `function`-typed entries; the
//! response's `tool_calls` are mapped to [`ContentBlock::ToolUse`] and the
//! parallel [`ToolCall`](crate::agent::llm::ToolCall) vector. Multi-turn tool
//! flows work end-to-end.
//!
//! Streaming: server-sent events (`stream=true`). Falls back to a single
//! `StreamEvent::Message` if the upstream doesn't support SSE (response
//! arrives as JSON). Each delta arrives as `TextDelta`; tool-call deltas
//! are buffered and emitted as a single `ToolUse` event when complete to
//! keep the contract stable across upstreams.

use async_trait::async_trait;
use futures_util::stream::{BoxStream, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

pub(crate) use super::openai_responses as responses_wire;

use crate::agent::llm::{
    ChatRequest, ChatResponse, ContentBlock, LlmError, Provider, Result, StreamEvent,
};
use crate::config::AgentConfig;

pub const PROVIDER_NAME: &str = "openai";

/// Names this provider answers to in the registry. Aliases share the core
/// wire format, while [`compatibility_for_alias`] gates optional OpenAI
/// request extensions that strict compatibility servers may reject.
pub const PROVIDER_ALIASES: &[&str] = &[
    "openai",
    "xai",
    "deepseek",
    "openrouter",
    "ollama",
    "azure",
    "copilot",
];

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_XAI_BASE: &str = "https://api.x.ai/v1";
const DEFAULT_DEEPSEEK_BASE: &str = "https://api.deepseek.com/v1";
const DEFAULT_OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";
const DEFAULT_OLLAMA_BASE: &str = "http://localhost:11434/v1";
// Azure has no universal default — every deployment lives at
// `https://<resource>.openai.azure.com/openai/deployments/<deployment>`.
// We return "" so empty-base callers fall through to a clear
// configuration error rather than silently 401'ing against api.openai.com.
const DEFAULT_AZURE_BASE: &str = "";
// GitHub Copilot's chat-completions endpoint. The Copilot API token
// embeds a `proxy-ep=` parameter that lets us route to per-tenant
// hosts (individual / business / enterprise) — see
// `super::copilot_auth::derive_base_url_from_token`. This constant is
// the fallback when no proxy-ep is present.
const DEFAULT_COPILOT_BASE: &str = "https://api.individual.githubcopilot.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AliasCompatibility {
    /// Whether Chat Completions accepts OpenAI's optional
    /// `stream_options.include_usage` request extension.
    chat_stream_usage: bool,
}

impl AliasCompatibility {
    const OFFICIAL_OPENAI: Self = Self {
        chat_stream_usage: true,
    };
    const STRICT: Self = Self {
        chat_stream_usage: false,
    };
}

/// Resolve optional wire extensions by the explicitly selected provider
/// alias. Unknown and compatibility aliases fail safe to the baseline
/// Chat Completions schema; only the official OpenAI alias opts in.
fn compatibility_for_alias(alias: &str) -> AliasCompatibility {
    if alias == PROVIDER_NAME {
        AliasCompatibility::OFFICIAL_OPENAI
    } else {
        AliasCompatibility::STRICT
    }
}

/// Resolve the default base URL for one of [`PROVIDER_ALIASES`]. Falls
/// back to OpenAI's URL if the alias is unknown.
pub fn default_base_url_for(alias: &str) -> &'static str {
    match alias {
        "xai" => DEFAULT_XAI_BASE,
        "deepseek" => DEFAULT_DEEPSEEK_BASE,
        "openrouter" => DEFAULT_OPENROUTER_BASE,
        "ollama" => DEFAULT_OLLAMA_BASE,
        "azure" => DEFAULT_AZURE_BASE,
        "copilot" => DEFAULT_COPILOT_BASE,
        _ => DEFAULT_OPENAI_BASE,
    }
}

/// Whether the alias authenticates via Azure's `api-key:` header
/// instead of the standard `Authorization: Bearer …`.
fn alias_uses_api_key_header(alias: &str) -> bool {
    matches!(alias, "azure")
}

/// Whether the alias's default base URL is local-only (no API key
/// required). Lets `is_configured()` return true for Ollama without a
/// stored key.
fn alias_is_local_default(alias: &str) -> bool {
    matches!(alias, "ollama")
}

/// Whether the alias treats the stored credential as a GitHub OAuth
/// token that must be exchanged for a short-lived Copilot API token on
/// every request (with in-process caching — see
/// `super::copilot_auth::ensure_copilot_token`). Aliases in this set
/// also inject Copilot's editor-identification headers and derive
/// their base URL from the exchanged token.
fn alias_is_copilot(alias: &str) -> bool {
    matches!(alias, "copilot")
}

/// Resolve an API key from the credential store, then env var, then None.
/// Missing and blank entries fall through; unreadable stored credentials
/// produce a typed error so construction cannot silently drop corruption.
pub fn resolve_api_key(
    api_key_credential: Option<&str>,
    api_key_env: Option<&str>,
) -> Result<Option<String>> {
    if let Some(name) = api_key_credential {
        match crate::credential::try_load(name, "agent").map_err(|message| {
            LlmError::CredentialStore {
                credential: name.to_string(),
                message,
            }
        })? {
            Some(value) => {
                let value = value.trim();
                if !value.is_empty() {
                    return Ok(Some(value.to_string()));
                }
            }
            None => {
                // Fall through to env.
            }
        }
    }
    if let Some(env_name) = api_key_env {
        if let Ok(value) = std::env::var(env_name) {
            let value = value.trim();
            if !value.is_empty() {
                return Ok(Some(value.to_string()));
            }
        }
    }
    Ok(None)
}

#[derive(Clone)]
pub struct OpenAICompatConfig {
    /// Stable name reported by [`Provider::name`] — one of [`PROVIDER_ALIASES`].
    pub alias: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub extra_headers: HashMap<String, String>,
    pub request_timeout: Duration,
    /// Optional multi-key credential pool. When `Some`, supersedes
    /// `api_key` per request: each `chat()` call acquires a lease,
    /// uses that as the bearer token, and reports success or failure
    /// (classified via [`crate::agent::llm::error_classifier`]). When
    /// `None`, the single `api_key` is used unchanged.
    pub pool: Option<Arc<crate::agent::llm::credential_pool::Pool>>,
}

impl std::fmt::Debug for OpenAICompatConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAICompatConfig")
            .field("alias", &self.alias)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("model", &self.model)
            .field(
                "extra_headers",
                &self.extra_headers.keys().collect::<Vec<_>>(),
            )
            .field("request_timeout", &self.request_timeout)
            .field("pool_len", &self.pool.as_ref().map(|p| p.len()))
            .finish()
    }
}

impl OpenAICompatConfig {
    /// Build from a registered alias + the agent config block.
    pub fn try_from_agent_config(alias: &str, model: &str, agent: &AgentConfig) -> Result<Self> {
        let base_url = agent
            .base_url
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default_base_url_for(alias).to_string());

        // Strip a trailing slash so the request path concat is clean.
        let base_url = base_url.trim_end_matches('/').to_string();

        let request_timeout = if agent.request_timeout == 0 {
            Duration::from_secs(0)
        } else {
            Duration::from_secs(agent.request_timeout)
        };

        // A declared pool is authoritative. Resolve it before touching the
        // legacy fields so stale single-key credentials can neither rescue
        // nor interfere with pool configuration.
        let pool = crate::agent::llm::credential_pool::Pool::try_from_agent_config(
            format!("provider:{alias}"),
            agent,
        )?
        .map(Arc::new);
        let api_key = if pool.is_some() {
            None
        } else {
            resolve_api_key(
                agent.api_key_credential.as_deref(),
                agent.api_key_env.as_deref(),
            )?
        };

        Ok(Self {
            alias: alias.to_string(),
            base_url,
            api_key,
            model: model.to_string(),
            extra_headers: agent.extra_headers.clone(),
            request_timeout,
            pool,
        })
    }

    #[cfg(test)]
    pub fn from_agent_config(alias: &str, model: &str, agent: &AgentConfig) -> Self {
        Self::try_from_agent_config(alias, model, agent)
            .expect("test credential configuration should resolve")
    }
}

pub struct OpenAICompatProvider {
    cfg: OpenAICompatConfig,
    client: reqwest::Client,
    copilot_auth: Arc<dyn CopilotAuthSource>,
}

struct RequestTarget {
    bearer: Option<String>,
    endpoint_url: String,
    wire_api: super::copilot_auth::CopilotWireApi,
    copilot: Option<CopilotRequestAuth>,
}

struct CopilotRequestAuth {
    github_token: String,
    token: super::copilot_auth::CopilotToken,
    refresh_used: bool,
}

type CopilotAuthResult<T> = std::result::Result<T, super::copilot_auth::CopilotAuthError>;

#[async_trait]
trait CopilotAuthSource: Send + Sync {
    async fn ensure_token(
        &self,
        github_token: &str,
    ) -> CopilotAuthResult<super::copilot_auth::CopilotToken>;

    async fn refresh_rejected_token(
        &self,
        github_token: &str,
        rejected_token: &super::copilot_auth::CopilotToken,
    ) -> CopilotAuthResult<super::copilot_auth::CopilotToken>;

    async fn wire_api_for_model(
        &self,
        token: &super::copilot_auth::CopilotToken,
        model: &str,
    ) -> CopilotAuthResult<super::copilot_auth::CopilotWireApi>;
}

struct LiveCopilotAuthSource;

#[async_trait]
impl CopilotAuthSource for LiveCopilotAuthSource {
    async fn ensure_token(
        &self,
        github_token: &str,
    ) -> CopilotAuthResult<super::copilot_auth::CopilotToken> {
        super::copilot_auth::ensure_copilot_token(github_token).await
    }

    async fn refresh_rejected_token(
        &self,
        github_token: &str,
        rejected_token: &super::copilot_auth::CopilotToken,
    ) -> CopilotAuthResult<super::copilot_auth::CopilotToken> {
        super::copilot_auth::refresh_rejected_copilot_token(github_token, rejected_token).await
    }

    async fn wire_api_for_model(
        &self,
        token: &super::copilot_auth::CopilotToken,
        model: &str,
    ) -> CopilotAuthResult<super::copilot_auth::CopilotWireApi> {
        super::copilot_auth::wire_api_for_model(token, model).await
    }
}

impl OpenAICompatProvider {
    pub fn new(cfg: OpenAICompatConfig) -> Self {
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("cos-agent/", env!("CARGO_PKG_VERSION")))
            // MEDIUM-14: per-phase timeouts. `request_timeout` covers
            // the whole call; `connect_timeout` bounds just the TCP +
            // TLS handshake so a black-holed DNS / firewalled host
            // can't tie up a worker for the full request budget.
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(60));
        if cfg.request_timeout > Duration::from_secs(0) {
            builder = builder.timeout(cfg.request_timeout);
        }
        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        Self {
            cfg,
            client,
            copilot_auth: Arc::new(LiveCopilotAuthSource),
        }
    }

    #[cfg(test)]
    fn new_with_copilot_auth_source(
        cfg: OpenAICompatConfig,
        copilot_auth: Arc<dyn CopilotAuthSource>,
    ) -> Self {
        let mut provider = Self::new(cfg);
        provider.copilot_auth = copilot_auth;
        provider
    }

    /// Convenience constructor that pulls everything from `AgentConfig`.
    /// Used by the registry.
    pub fn try_from_agent_config(alias: &str, model: &str, agent: &AgentConfig) -> Result<Self> {
        Ok(Self::new(OpenAICompatConfig::try_from_agent_config(
            alias, model, agent,
        )?))
    }

    #[cfg(test)]
    pub fn from_agent_config(alias: &str, model: &str, agent: &AgentConfig) -> Self {
        Self::try_from_agent_config(alias, model, agent)
            .expect("test credential configuration should resolve")
    }

    fn endpoint(&self) -> String {
        // Azure OpenAI uses a different URL shape than every other
        // openai-compat provider: the stored `base_url` is the
        // *resource root* (e.g. `https://acme.openai.azure.com/`),
        // the deployment name is the `model` field, and the api
        // version is required as a query string. The official
        // Python/JS SDKs take exactly the same two pieces of input
        // (`azure_endpoint` + `deployment`) and assemble the path
        // internally — we do the same so the prompt the wizard
        // shows the user matches what they see in the Azure portal.
        if self.cfg.alias == "azure" {
            let (base, query) = match self.cfg.base_url.split_once('?') {
                Some((b, q)) => (b.trim_end_matches('/'), Some(q)),
                None => (self.cfg.base_url.trim_end_matches('/'), None),
            };
            let deployment = self.cfg.model.as_str();
            return match query {
                Some(q) => {
                    format!("{base}/openai/deployments/{deployment}/chat/completions?{q}")
                }
                None => format!("{base}/openai/deployments/{deployment}/chat/completions"),
            };
        }

        // Generic openai-compat: stored base_url already includes
        // the API version path (e.g. `/v1`), so we just append
        // `/chat/completions` plus any trailing query string the
        // user may have configured for a proxy.
        endpoint_from_base(&self.cfg.base_url)
    }

    async fn request_target(
        &self,
        lease: Option<&crate::agent::llm::credential_pool::Lease>,
    ) -> Result<RequestTarget> {
        if alias_is_copilot(&self.cfg.alias) {
            let github_token = match lease {
                Some(lease) => lease.value().to_string(),
                None => self.cfg.api_key.clone().ok_or_else(|| {
                    LlmError::NotConfigured(
                        "GitHub Copilot is not signed in. Run \
                         `cos agent setup text oauth-start --provider copilot` \
                         or use the desktop AI settings page to sign in with GitHub."
                            .into(),
                    )
                })?,
            };
            let token = self
                .copilot_auth
                .ensure_token(&github_token)
                .await
                .map_err(map_copilot_error)?;
            return self
                .copilot_request_target(github_token, token, false)
                .await;
        }

        Ok(RequestTarget {
            bearer: lease
                .map(|lease| lease.value().to_string())
                .or_else(|| self.cfg.api_key.clone()),
            endpoint_url: self.endpoint(),
            wire_api: super::copilot_auth::CopilotWireApi::ChatCompletions,
            copilot: None,
        })
    }

    async fn copilot_request_target(
        &self,
        github_token: String,
        mut token: super::copilot_auth::CopilotToken,
        mut refresh_used: bool,
    ) -> Result<RequestTarget> {
        loop {
            match self
                .copilot_auth
                .wire_api_for_model(&token, &self.cfg.model)
                .await
            {
                Ok(wire_api) => {
                    return Ok(RequestTarget {
                        bearer: Some(token.bearer.clone()),
                        endpoint_url: endpoint_for_wire_api(&token.base_url, wire_api),
                        wire_api,
                        copilot: Some(CopilotRequestAuth {
                            github_token,
                            token,
                            refresh_used,
                        }),
                    });
                }
                Err(error) if copilot_api_rejected_token(&error) && !refresh_used => {
                    token = self
                        .copilot_auth
                        .refresh_rejected_token(&github_token, &token)
                        .await
                        .map_err(map_copilot_error)?;
                    refresh_used = true;
                }
                Err(error) => return Err(map_copilot_error(error)),
            }
        }
    }

    async fn refresh_request_target(
        &self,
        target: &RequestTarget,
    ) -> Result<Option<RequestTarget>> {
        let Some(auth) = target.copilot.as_ref() else {
            return Ok(None);
        };
        if auth.refresh_used {
            return Ok(None);
        }
        let token = self
            .copilot_auth
            .refresh_rejected_token(&auth.github_token, &auth.token)
            .await
            .map_err(map_copilot_error)?;
        self.copilot_request_target(auth.github_token.clone(), token, true)
            .await
            .map(Some)
    }
}

/// Build the `/chat/completions` URL from a generic openai-compat base
/// URL, preserving any trailing `?…` query string the user may have
/// configured (proxy auth tokens, region selectors, …).
fn endpoint_from_base(base_url: &str) -> String {
    endpoint_for_wire_api(
        base_url,
        super::copilot_auth::CopilotWireApi::ChatCompletions,
    )
}

fn endpoint_for_wire_api(base_url: &str, wire_api: super::copilot_auth::CopilotWireApi) -> String {
    let (base, query) = match base_url.split_once('?') {
        Some((b, q)) => (b.trim_end_matches('/'), Some(q)),
        None => (base_url.trim_end_matches('/'), None),
    };
    let path = wire_api.endpoint_path();
    match query {
        Some(q) => format!("{base}{path}?{q}"),
        None => format!("{base}{path}"),
    }
}

fn map_copilot_error(error: super::copilot_auth::CopilotAuthError) -> LlmError {
    use super::copilot_auth::CopilotAuthError;
    match error {
        CopilotAuthError::UnsupportedModel(message) => LlmError::InvalidRequest(message),
        CopilotAuthError::Http {
            status: 401 | 403, ..
        } => LlmError::Auth,
        CopilotAuthError::Http { status, body } => LlmError::Provider {
            status,
            message: crate::agent::llm::redact_body_for_error(&body),
        },
        CopilotAuthError::Network(message) => {
            LlmError::NotConfigured(format!("copilot model discovery failed: {message}"))
        }
        CopilotAuthError::UnexpectedBody(message) => LlmError::UpstreamMalformed(message),
        CopilotAuthError::NotAuthorized(message) => LlmError::NotConfigured(message),
    }
}

fn copilot_api_rejected_token(error: &super::copilot_auth::CopilotAuthError) -> bool {
    matches!(
        error,
        super::copilot_auth::CopilotAuthError::Http {
            status: 401 | 403,
            ..
        }
    )
}

fn pool_failure_class(error: &LlmError) -> crate::agent::llm::credential_pool::FailureClass {
    use crate::agent::llm::credential_pool::FailureClass;
    match error {
        LlmError::Auth | LlmError::RateLimited { .. } => FailureClass::CooldownWorthy,
        LlmError::Provider { status, .. } if matches!(*status, 401 | 403 | 429) => {
            FailureClass::CooldownWorthy
        }
        LlmError::Provider { status, .. } if *status >= 500 => FailureClass::Transient,
        LlmError::Transport(_)
        | LlmError::UpstreamMalformed(_)
        | LlmError::Stream(_)
        | LlmError::Internal(_) => FailureClass::Transient,
        _ => FailureClass::CallerError,
    }
}

fn request_has_image(request: &ChatRequest) -> bool {
    request.messages.iter().any(|message| {
        message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image { .. }))
    })
}

fn build_wire_request_body(
    request: &ChatRequest,
    model: &str,
    stream: bool,
    wire_api: super::copilot_auth::CopilotWireApi,
    compatibility: AliasCompatibility,
) -> Result<serde_json::Value> {
    match wire_api {
        super::copilot_auth::CopilotWireApi::ChatCompletions => {
            if compatibility.chat_stream_usage {
                wire::build_request_body_with_stream_usage(request, model, stream)
            } else {
                wire::build_request_body(request, model, stream)
            }
        }
        super::copilot_auth::CopilotWireApi::Responses => {
            Ok(responses_wire::build_request_body(request, model, stream))
        }
    }
}

fn copilot_initiator(request: &ChatRequest) -> &'static str {
    if request
        .extra
        .get("_cos_initiator")
        .and_then(|value| value.as_str())
        == Some("agent")
    {
        return super::copilot_auth::COPILOT_INITIATOR_AGENT;
    }
    let is_tool_follow_up = request.messages.last().is_some_and(|message| {
        !message.content.is_empty()
            && message
                .content
                .iter()
                .all(|block| matches!(block, ContentBlock::ToolResult { .. }))
    });
    if is_tool_follow_up {
        super::copilot_auth::COPILOT_INITIATOR_AGENT
    } else {
        super::copilot_auth::COPILOT_INITIATOR_USER
    }
}

fn with_copilot_headers(
    mut request: reqwest::RequestBuilder,
    vision_request: bool,
    initiator: &'static str,
) -> reqwest::RequestBuilder {
    request = request
        .header("Editor-Version", super::copilot_auth::EDITOR_VERSION)
        .header(
            "Copilot-Integration-Id",
            super::copilot_auth::COPILOT_INTEGRATION_ID,
        )
        .header(
            "X-GitHub-Api-Version",
            super::copilot_auth::GITHUB_API_VERSION,
        )
        .header("X-Initiator", initiator)
        .header(
            "X-Interaction-Type",
            super::copilot_auth::COPILOT_INTERACTION_TYPE,
        )
        .header(
            "OpenAI-Intent",
            super::copilot_auth::COPILOT_INTERACTION_TYPE,
        );
    if vision_request {
        request = request.header("Copilot-Vision-Request", "true");
    }
    request
}

#[async_trait]
impl Provider for OpenAICompatProvider {
    fn name(&self) -> &str {
        // Borrow from the owned config — keeps the trait's borrow lifetime.
        self.cfg.alias.as_str()
    }

    fn supported_models(&self) -> Vec<String> {
        vec![self.cfg.model.clone()]
    }

    fn is_configured(&self) -> bool {
        // Azure requires both a key and the deployment URL — no
        // sensible default base.
        if self.cfg.alias == "azure" && self.cfg.base_url.is_empty() {
            return false;
        }
        self.cfg.api_key.is_some()
            || self.cfg.pool.as_ref().is_some_and(|p| !p.is_empty())
            || alias_is_local_default(&self.cfg.alias)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        if self.cfg.alias == "azure" && self.cfg.base_url.is_empty() {
            return Err(LlmError::NotConfigured(
                "azure provider needs `agent.base_url` set to the Azure OpenAI \
                 resource root (e.g. https://<resource>.openai.azure.com/). The \
                 model field is treated as the deployment name. Run \
                 `cos agent setup text apply --provider azure \
                 --base-url https://<resource>.openai.azure.com/ \
                 --model <deployment> --api-version <version> \
                 --api-key-stdin`."
                    .into(),
            ));
        }
        // A declared pool is authoritative. Its lease snapshots the value so
        // concurrent cooldown updates do not invalidate this call.
        let lease = if let Some(pool) = &self.cfg.pool {
            match pool.acquire() {
                Ok(l) => Some(l),
                Err(e) => return Err(LlmError::NotConfigured(format!("pool: {e}"))),
            }
        } else {
            None
        };

        let vision_request = request_has_image(&request);
        let initiator = copilot_initiator(&request);
        let mut target = match self.request_target(lease.as_ref()).await {
            Ok(target) => target,
            Err(error) => {
                if let (Some(pool), Some(lease)) = (&self.cfg.pool, &lease) {
                    pool.report_failure(lease, pool_failure_class(&error));
                }
                return Err(error);
            }
        };

        loop {
            let body = match build_wire_request_body(
                &request,
                &self.cfg.model,
                false,
                target.wire_api,
                compatibility_for_alias(&self.cfg.alias),
            ) {
                Ok(body) => body,
                Err(error) => {
                    if let (Some(pool), Some(lease)) = (&self.cfg.pool, &lease) {
                        pool.report_failure(lease, pool_failure_class(&error));
                    }
                    return Err(error);
                }
            };

            let mut http = self
                .client
                .post(&target.endpoint_url)
                .header("Content-Type", "application/json")
                .json(&body);

            if let Some(key) = target.bearer.as_deref() {
                if alias_uses_api_key_header(&self.cfg.alias) {
                    http = http.header("api-key", key);
                } else {
                    http = http.bearer_auth(key);
                }
            }
            // Copilot's API rejects requests without these two headers
            // (and uses them for telemetry + entitlement gating). They
            // must come AFTER `extra_headers` is applied so a user's
            // `agent.extra_headers` cannot accidentally clobber them.
            for (k, v) in &self.cfg.extra_headers {
                http = http.header(k.as_str(), v.as_str());
            }
            if alias_is_copilot(&self.cfg.alias) {
                http = with_copilot_headers(http, vision_request, initiator);
            }

            let resp = match http.send().await {
                Ok(r) => r,
                Err(e) => {
                    if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                        pool.report_failure(
                            l,
                            crate::agent::llm::error_classifier::classify_network_error(),
                        );
                    }
                    return Err(LlmError::Transport(e));
                }
            };
            let status = resp.status();
            // SECURITY: cap the response body so a hostile upstream can't
            // OOM us with a multi-GiB blob (HIGH-5).
            let body_result = crate::agent::llm::read_body_capped(
                resp,
                crate::agent::llm::MAX_NONSTREAM_BODY_BYTES,
            )
            .await;

            if !status.is_success() && matches!(status.as_u16(), 401 | 403) {
                match self.refresh_request_target(&target).await {
                    Ok(Some(refreshed)) => {
                        target = refreshed;
                        continue;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                            pool.report_failure(l, pool_failure_class(&error));
                        }
                        return Err(error);
                    }
                }
            }

            let bytes = match body_result {
                Ok(b) => b,
                Err(_) if target.copilot.is_some() && matches!(status.as_u16(), 401 | 403) => {
                    bytes::Bytes::new()
                }
                Err(e) => {
                    if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                        let cls = match &e {
                            LlmError::UpstreamMalformed(_) => {
                                crate::agent::llm::credential_pool::FailureClass::Transient
                            }
                            _ => crate::agent::llm::error_classifier::classify_network_error(),
                        };
                        pool.report_failure(l, cls);
                    }
                    return Err(e);
                }
            };

            if !status.is_success() {
                let err = wire::classify_http_error(status, &bytes);
                if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                    let body_str = std::str::from_utf8(&bytes).unwrap_or("");
                    let cls =
                        crate::agent::llm::error_classifier::classify(status.as_u16(), body_str);
                    pool.report_failure(l, cls);
                }
                return Err(err);
            }

            let result = match target.wire_api {
                super::copilot_auth::CopilotWireApi::ChatCompletions => {
                    serde_json::from_slice::<wire::Response>(&bytes)
                        .map_err(|e| LlmError::Parse(e.to_string()))
                        .and_then(|parsed| wire::response_to_chat(parsed, &self.cfg.model))
                }
                super::copilot_auth::CopilotWireApi::Responses => {
                    responses_wire::response_from_slice(&bytes, &self.cfg.model)
                }
            };
            if result.is_ok() {
                if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                    pool.report_success(l);
                }
            }
            return result;
        }
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        // HIGH-4: real SSE delta streaming. Build the request body
        // with stream:true, POST it, validate the HTTP status
        // synchronously (so 401/429/5xx surface immediately), then
        // wrap the body's bytes_stream in the OpenAI SSE converter
        // below, which emits TextDelta / ToolUseStart /
        // ToolInputDelta / ToolUse / Done events as the upstream
        // deltas arrive.
        if self.cfg.alias == "azure" && self.cfg.base_url.is_empty() {
            return Err(LlmError::NotConfigured(
                "azure provider needs `agent.base_url` set".into(),
            ));
        }
        let lease = if let Some(pool) = &self.cfg.pool {
            match pool.acquire() {
                Ok(l) => Some(l),
                Err(e) => return Err(LlmError::NotConfigured(format!("pool: {e}"))),
            }
        } else {
            None
        };

        let vision_request = request_has_image(&request);
        let initiator = copilot_initiator(&request);
        let mut target = match self.request_target(lease.as_ref()).await {
            Ok(target) => target,
            Err(error) => {
                if let (Some(pool), Some(lease)) = (&self.cfg.pool, &lease) {
                    pool.report_failure(lease, pool_failure_class(&error));
                }
                return Err(error);
            }
        };

        loop {
            let body = match build_wire_request_body(
                &request,
                &self.cfg.model,
                true,
                target.wire_api,
                compatibility_for_alias(&self.cfg.alias),
            ) {
                Ok(body) => body,
                Err(error) => {
                    if let (Some(pool), Some(lease)) = (&self.cfg.pool, &lease) {
                        pool.report_failure(lease, pool_failure_class(&error));
                    }
                    return Err(error);
                }
            };

            let mut http = self
                .client
                .post(&target.endpoint_url)
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                .json(&body);

            if let Some(key) = target.bearer.as_deref() {
                if alias_uses_api_key_header(&self.cfg.alias) {
                    http = http.header("api-key", key);
                } else {
                    http = http.bearer_auth(key);
                }
            }
            for (k, v) in &self.cfg.extra_headers {
                http = http.header(k.as_str(), v.as_str());
            }
            if alias_is_copilot(&self.cfg.alias) {
                http = with_copilot_headers(http, vision_request, initiator);
            }

            let resp = match http.send().await {
                Ok(r) => r,
                Err(e) => {
                    if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                        pool.report_failure(
                            l,
                            crate::agent::llm::error_classifier::classify_network_error(),
                        );
                    }
                    return Err(LlmError::Transport(e));
                }
            };

            let status = resp.status();
            if !status.is_success() {
                // Reuse the body cap so an error response can't OOM us
                // either.
                let bytes = crate::agent::llm::read_body_capped(
                    resp,
                    crate::agent::llm::MAX_NONSTREAM_BODY_BYTES,
                )
                .await
                .unwrap_or_default();
                if matches!(status.as_u16(), 401 | 403) {
                    match self.refresh_request_target(&target).await {
                        Ok(Some(refreshed)) => {
                            target = refreshed;
                            continue;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                                pool.report_failure(l, pool_failure_class(&error));
                            }
                            return Err(error);
                        }
                    }
                }
                let err = wire::classify_http_error(status, &bytes);
                if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                    let body_str = std::str::from_utf8(&bytes).unwrap_or("");
                    let cls =
                        crate::agent::llm::error_classifier::classify(status.as_u16(), body_str);
                    pool.report_failure(l, cls);
                }
                return Err(err);
            }

            // MEDIUM-9 / streaming success accounting: credit the lease
            // only when the body's `[DONE]` event lands, not here on
            // headers. Pass the lease into the stream so it can call
            // report_success / report_failure at the right boundary.
            let bytes_stream = resp.bytes_stream();
            let model = self.cfg.model.clone();
            return match target.wire_api {
                super::copilot_auth::CopilotWireApi::ChatCompletions => Ok(
                    wire::OpenAiStream::new(bytes_stream, model, self.cfg.pool.clone(), lease)
                        .boxed(),
                ),
                super::copilot_auth::CopilotWireApi::Responses => {
                    Ok(responses_wire::ResponsesStream::new(
                        bytes_stream,
                        model,
                        self.cfg.pool.clone(),
                        lease,
                    )
                    .boxed())
                }
            };
        }
    }
}

pub(crate) use super::openai_chat as wire;

// Free function so the registry can decide whether the alias is one we own.
pub fn is_alias(name: &str) -> bool {
    PROVIDER_ALIASES.contains(&name)
}

// Construction helper used by the registry.
pub fn build_provider(alias: &str, model: &str, agent: &AgentConfig) -> Result<Arc<dyn Provider>> {
    Ok(Arc::new(OpenAICompatProvider::try_from_agent_config(
        alias, model, agent,
    )?))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/providers/openai_compat.rs"
    ));
}
