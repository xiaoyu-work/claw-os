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
//! parallel [`ToolCall`] vector. Multi-turn tool flows work end-to-end.
//!
//! Streaming: server-sent events (`stream=true`). Falls back to a single
//! `StreamEvent::Message` if the upstream doesn't support SSE (response
//! arrives as JSON). Each delta arrives as `TextDelta`; tool-call deltas
//! are buffered and emitted as a single `ToolUse` event when complete to
//! keep the contract stable across upstreams.

use async_trait::async_trait;
use futures_util::stream::{BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

pub(crate) use super::openai_responses as responses_wire;

use crate::agent::llm::{
    ChatRequest, ChatResponse, ContentBlock, FinishReason, LlmError, Provider, Result, Role,
    StreamEvent, Tool, ToolCall, ToolChoice, Usage,
};
use crate::config::AgentConfig;

pub const PROVIDER_NAME: &str = "openai";

/// Names this provider answers to in the registry. Adding an alias here
/// only changes the `name()` returned and the default base URL — the
/// wire format is identical.
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
/// Errors only on a corrupted credential file — a missing entry is `Ok(None)`.
pub fn resolve_api_key(
    api_key_credential: Option<&str>,
    api_key_env: Option<&str>,
) -> std::result::Result<Option<String>, String> {
    if let Some(name) = api_key_credential {
        match crate::credential::try_load(name, "agent")? {
            Some(value) => return Ok(Some(value)),
            None => {
                // Fall through to env.
            }
        }
    }
    if let Some(env_name) = api_key_env {
        if let Ok(value) = std::env::var(env_name) {
            if !value.is_empty() {
                return Ok(Some(value));
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
    pub fn from_agent_config(alias: &str, model: &str, agent: &AgentConfig) -> Self {
        let base_url = agent
            .base_url
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default_base_url_for(alias).to_string());

        // Strip a trailing slash so the request path concat is clean.
        let base_url = base_url.trim_end_matches('/').to_string();

        let api_key = resolve_api_key(
            agent.api_key_credential.as_deref(),
            agent.api_key_env.as_deref(),
        )
        .ok()
        .flatten();

        let request_timeout = if agent.request_timeout == 0 {
            Duration::from_secs(0)
        } else {
            Duration::from_secs(agent.request_timeout)
        };

        // Pool supersedes single-key when multi-key fields are set.
        // Pool construction failure (declared but unresolved) is logged
        // and ignored — fall through to single-key. This keeps a typo
        // in `api_key_credentials` from bricking the agent at startup.
        let pool = match crate::agent::llm::credential_pool::Pool::try_from_agent_config(
            format!("provider:{alias}"),
            agent,
        ) {
            Ok(Some(p)) => Some(Arc::new(p)),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(
                    target: "cos::agent::llm::pool",
                    "credential pool for provider '{alias}' declared but unresolved: {e}; \
                     falling back to single-key path"
                );
                None
            }
        };

        Self {
            alias: alias.to_string(),
            base_url,
            api_key,
            model: model.to_string(),
            extra_headers: agent.extra_headers.clone(),
            request_timeout,
            pool,
        }
    }
}

pub struct OpenAICompatProvider {
    cfg: OpenAICompatConfig,
    client: reqwest::Client,
}

struct RequestTarget {
    bearer: Option<String>,
    endpoint_url: String,
    wire_api: super::copilot_auth::CopilotWireApi,
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
        Self { cfg, client }
    }

    /// Convenience constructor that pulls everything from `AgentConfig`.
    /// Used by the registry.
    pub fn from_agent_config(alias: &str, model: &str, agent: &AgentConfig) -> Self {
        Self::new(OpenAICompatConfig::from_agent_config(alias, model, agent))
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
                         `cos agent setup llm oauth-start --provider copilot` \
                         or use the desktop AI settings page to sign in with GitHub."
                            .into(),
                    )
                })?,
            };
            let token = super::copilot_auth::ensure_copilot_token(&github_token)
                .await
                .map_err(map_copilot_error)?;
            let wire_api = super::copilot_auth::wire_api_for_model(&token, &self.cfg.model)
                .await
                .map_err(map_copilot_error)?;
            return Ok(RequestTarget {
                bearer: Some(token.bearer),
                endpoint_url: endpoint_for_wire_api(&token.base_url, wire_api),
                wire_api,
            });
        }

        Ok(RequestTarget {
            bearer: lease
                .map(|lease| lease.value().to_string())
                .or_else(|| self.cfg.api_key.clone()),
            endpoint_url: self.endpoint(),
            wire_api: super::copilot_auth::CopilotWireApi::ChatCompletions,
        })
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
                 `cos agent setup llm apply --provider azure \
                 --base-url https://<resource>.openai.azure.com/ \
                 --model <deployment> --api-version <version> \
                 --api-key-stdin`."
                    .into(),
            ));
        }
        // Acquire a key for this call. Pool path takes priority; on
        // empty pool fall through to single-key. Lease holds the
        // snapshotted value so concurrent cooldown bumps don't
        // invalidate it.
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
        let target = match self.request_target(lease.as_ref()).await {
            Ok(target) => target,
            Err(error) => {
                if let (Some(pool), Some(lease)) = (&self.cfg.pool, &lease) {
                    pool.report_failure(lease, pool_failure_class(&error));
                }
                return Err(error);
            }
        };
        let body = match target.wire_api {
            super::copilot_auth::CopilotWireApi::ChatCompletions => {
                wire::build_request_body(&request, &self.cfg.model, false)
            }
            super::copilot_auth::CopilotWireApi::Responses => {
                responses_wire::build_request_body(&request, &self.cfg.model, false)
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

        let send_result = http.send().await;
        let resp = match send_result {
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
        let bytes = match crate::agent::llm::read_body_capped(
            resp,
            crate::agent::llm::MAX_NONSTREAM_BODY_BYTES,
        )
        .await
        {
            Ok(b) => b,
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
                let cls = crate::agent::llm::error_classifier::classify(status.as_u16(), body_str);
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
        result
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
        let target = match self.request_target(lease.as_ref()).await {
            Ok(target) => target,
            Err(error) => {
                if let (Some(pool), Some(lease)) = (&self.cfg.pool, &lease) {
                    pool.report_failure(lease, pool_failure_class(&error));
                }
                return Err(error);
            }
        };
        let body = match target.wire_api {
            super::copilot_auth::CopilotWireApi::ChatCompletions => {
                wire::build_request_body(&request, &self.cfg.model, true)
            }
            super::copilot_auth::CopilotWireApi::Responses => {
                responses_wire::build_request_body(&request, &self.cfg.model, true)
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
            let err = wire::classify_http_error(status, &bytes);
            if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                let body_str = std::str::from_utf8(&bytes).unwrap_or("");
                let cls = crate::agent::llm::error_classifier::classify(status.as_u16(), body_str);
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
        match target.wire_api {
            super::copilot_auth::CopilotWireApi::ChatCompletions => {
                Ok(
                    wire::OpenAiStream::new(bytes_stream, model, self.cfg.pool.clone(), lease)
                        .boxed(),
                )
            }
            super::copilot_auth::CopilotWireApi::Responses => {
                Ok(responses_wire::ResponsesStream::new(
                    bytes_stream,
                    model,
                    self.cfg.pool.clone(),
                    lease,
                )
                .boxed())
            }
        }
    }
}

/// Wire-format adapters: serialise our internal types into OpenAI's
/// chat-completions schema and parse responses back. Kept private and
/// pure (no IO) so we can unit-test it without spinning up an HTTP
/// server.
pub(crate) mod wire {
    use super::*;

    // --- Request --------------------------------------------------------

    #[derive(Debug, Serialize)]
    pub(crate) struct OutgoingMessage<'a> {
        pub role: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_calls: Option<Vec<OutgoingToolCall<'a>>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_call_id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub name: Option<&'a str>,
    }

    #[derive(Debug, Serialize)]
    pub(crate) struct OutgoingToolCall<'a> {
        pub id: &'a str,
        #[serde(rename = "type")]
        pub type_: &'static str,
        pub function: OutgoingFunctionCall<'a>,
    }

    #[derive(Debug, Serialize)]
    pub(crate) struct OutgoingFunctionCall<'a> {
        pub name: &'a str,
        pub arguments: String,
    }

    #[derive(Debug, Serialize)]
    pub(crate) struct OutgoingTool<'a> {
        #[serde(rename = "type")]
        pub type_: &'static str,
        pub function: OutgoingFunctionDef<'a>,
    }

    #[derive(Debug, Serialize)]
    pub(crate) struct OutgoingFunctionDef<'a> {
        pub name: &'a str,
        pub description: &'a str,
        pub parameters: &'a serde_json::Value,
    }

    /// Newer OpenAI models (o-series, gpt-5+) reject `max_tokens` and
    /// require `max_completion_tokens`. Older models (gpt-4o, gpt-4.1,
    /// claude-via-openrouter, deepseek, ollama) still expect
    /// `max_tokens`. Heuristic by model name prefix.
    pub(crate) fn use_max_completion_tokens(model: &str) -> bool {
        let m = model.to_ascii_lowercase();
        // Strip Azure deployment suffixes / variants like "gpt-5.4-mini"
        // → still starts with "gpt-5". Match on the family prefix.
        m.starts_with("gpt-5")
            || m.starts_with("gpt-6")
            || m.starts_with("o1")
            || m.starts_with("o3")
            || m.starts_with("o4")
    }

    /// Build the JSON body for `POST /v1/chat/completions`. Pure — no IO.
    pub(crate) fn build_request_body(
        request: &ChatRequest,
        model: &str,
        stream: bool,
    ) -> serde_json::Value {
        let mut messages: Vec<serde_json::Value> = Vec::with_capacity(request.messages.len() + 1);
        if let Some(sys) = &request.system {
            messages.push(serde_json::json!({ "role": "system", "content": sys }));
        }
        for m in &request.messages {
            for v in message_to_json_many(m) {
                messages.push(v);
            }
        }

        let tools: Vec<serde_json::Value> = request.tools.iter().map(tool_to_json).collect();

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
        });

        let modern = use_max_completion_tokens(model);

        if let Some(obj) = body.as_object_mut() {
            if !tools.is_empty() {
                obj.insert("tools".into(), serde_json::Value::Array(tools));
                obj.insert(
                    "tool_choice".into(),
                    tool_choice_to_json(&request.tool_choice),
                );
            }
            if let Some(v) = request.max_tokens {
                let key = if modern {
                    "max_completion_tokens"
                } else {
                    "max_tokens"
                };
                obj.insert(key.into(), serde_json::json!(v));
            }
            if let Some(v) = request.temperature {
                // o-series / gpt-5 only support the default temperature
                // (1.0). Sending any other value yields a 400. Skip the
                // field entirely for those models.
                if !modern {
                    obj.insert("temperature".into(), serde_json::json!(v));
                }
            }
            if let Some(v) = request.top_p {
                if !modern {
                    obj.insert("top_p".into(), serde_json::json!(v));
                }
            }
            if !request.stop_sequences.is_empty() {
                obj.insert("stop".into(), serde_json::json!(request.stop_sequences));
            }
            if stream {
                obj.insert("stream".into(), serde_json::json!(true));
            }
            // Merge provider-specific extras (e.g. `seed`, `response_format`).
            if let serde_json::Value::Object(extra) = &request.extra {
                for (k, v) in extra {
                    if k.starts_with("_cos_") {
                        continue;
                    }
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
        body
    }

    fn role_to_str(role: Role) -> &'static str {
        match role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }

    fn message_to_json(m: &crate::agent::llm::Message) -> serde_json::Value {
        // Back-compat single-output wrapper for tests that pre-date
        // the multi-tool-result fan-out. Returns the first emitted
        // wire message; the request path uses `message_to_json_many`
        // directly so multi-result messages are handled correctly.
        let mut all = message_to_json_many(m);
        if all.is_empty() {
            serde_json::json!({"role": role_to_str(m.role), "content": ""})
        } else {
            all.remove(0)
        }
    }

    /// Translate one runtime Message into one or more OpenAI-style
    /// wire messages.
    ///
    /// OpenAI's schema requires that each tool result is its own
    /// message with `role=tool` + `tool_call_id`. The runtime
    /// aggregates all tool results for a given assistant turn into a
    /// single `User` message holding `Vec<ContentBlock::ToolResult>`
    /// (see `runtime/turn.rs`). Translating that to a single wire
    /// message would silently drop the second+ ToolResult, leaving
    /// the conversation history malformed (assistant.tool_calls with
    /// no matching tool messages) — which Azure rejects with a 400.
    ///
    /// We fan out: a User message that consists *only* of
    /// ToolResult blocks becomes N separate `role=tool` messages
    /// preserving their order. All other messages map 1:1.
    fn message_to_json_many(m: &crate::agent::llm::Message) -> Vec<serde_json::Value> {
        let role = role_to_str(m.role);

        // Multi-tool-result fan-out: any message whose blocks are
        // *all* ToolResult is split into N tool messages.
        if !m.content.is_empty()
            && m.content
                .iter()
                .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
        {
            return m
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => Some(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "content": content,
                    })),
                    _ => None,
                })
                .collect();
        }

        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<serde_json::Value> = Vec::new();
        for block in &m.content {
            match block {
                ContentBlock::Text { text } => text_parts.push(text.clone()),
                ContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(serde_json::json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
                        }
                    }));
                }
                ContentBlock::ToolResult { .. } => {
                    // Pure-tool-result messages were fanned out above.
                    // A mixed message containing a ToolResult would
                    // be malformed input; we drop the result here
                    // rather than silently emit a broken user
                    // message. (Should not happen with the current
                    // runtime.)
                }
                ContentBlock::Reasoning { .. } => {
                    // Responses-only provider state must not leak into the
                    // Chat Completions wire format.
                }
                ContentBlock::ToolState { .. } => {
                    // Copilot-specific function-call metadata is only valid
                    // on the Responses wire format.
                }
                ContentBlock::Image { media_type, data } => {
                    text_parts.push(format!("[image {} base64 attached]", media_type));
                    let _ = data;
                }
            }
        }

        let mut obj = serde_json::Map::new();
        obj.insert("role".into(), serde_json::json!(role));
        if !text_parts.is_empty() {
            obj.insert("content".into(), serde_json::json!(text_parts.join("\n")));
        } else if tool_calls.is_empty() {
            obj.insert("content".into(), serde_json::json!(""));
        }
        if !tool_calls.is_empty() {
            obj.insert("tool_calls".into(), serde_json::Value::Array(tool_calls));
        }
        vec![serde_json::Value::Object(obj)]
    }

    fn tool_to_json(t: &Tool) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": t.name,
                "description": t.description,
                "parameters": t.input_schema,
            }
        })
    }

    fn tool_choice_to_json(c: &ToolChoice) -> serde_json::Value {
        match c {
            ToolChoice::Auto => serde_json::json!("auto"),
            ToolChoice::None => serde_json::json!("none"),
            ToolChoice::Required => serde_json::json!("required"),
            ToolChoice::Tool { name } => serde_json::json!({
                "type": "function",
                "function": { "name": name }
            }),
        }
    }

    // --- Response -------------------------------------------------------

    #[derive(Debug, Deserialize)]
    pub(crate) struct Response {
        #[serde(default)]
        pub model: Option<String>,
        pub choices: Vec<Choice>,
        #[serde(default)]
        pub usage: Option<UsageJson>,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct Choice {
        pub message: ChoiceMessage,
        #[serde(default)]
        pub finish_reason: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct ChoiceMessage {
        #[serde(default)]
        pub content: Option<String>,
        #[serde(default)]
        pub tool_calls: Vec<IncomingToolCall>,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct IncomingToolCall {
        pub id: String,
        #[serde(default)]
        pub function: IncomingFunctionCall,
    }

    #[derive(Debug, Default, Deserialize)]
    pub(crate) struct IncomingFunctionCall {
        #[serde(default)]
        pub name: String,
        #[serde(default)]
        pub arguments: String,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct UsageJson {
        #[serde(default)]
        pub prompt_tokens: u32,
        #[serde(default)]
        pub completion_tokens: u32,
    }

    pub(crate) fn response_to_chat(resp: Response, fallback_model: &str) -> Result<ChatResponse> {
        let choice = resp
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::Parse("response had no choices".into()))?;

        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        if let Some(text) = choice.message.content.filter(|s| !s.is_empty()) {
            content_blocks.push(ContentBlock::Text { text });
        }

        for tc in choice.message.tool_calls {
            // MEDIUM-11: An upstream that returns malformed JSON in
            // `function.arguments` used to silently null-out the
            // payload, hiding bugs that would later surface deep
            // inside the tool runner. Empty arguments are legal
            // (the tool takes no input); anything else must parse.
            let args_raw = tc.function.arguments.trim();
            let parsed: serde_json::Value = if args_raw.is_empty() {
                serde_json::Value::Object(serde_json::Map::new())
            } else {
                match serde_json::from_str(args_raw) {
                    Ok(v) => v,
                    Err(e) => {
                        return Err(LlmError::UpstreamMalformed(format!(
                            "tool_calls[{name}].arguments is not valid JSON: {err}",
                            name = tc.function.name,
                            err = e
                        )));
                    }
                }
            };
            content_blocks.push(ContentBlock::ToolUse {
                id: tc.id.clone(),
                name: tc.function.name.clone(),
                input: parsed.clone(),
            });
            tool_calls.push(ToolCall {
                id: tc.id,
                name: tc.function.name,
                input: parsed,
            });
        }

        let finish_reason = match choice.finish_reason.as_deref() {
            Some("stop") | Some("end_turn") | None => FinishReason::Stop,
            Some("length") | Some("max_tokens") => FinishReason::Length,
            Some("tool_calls") | Some("function_call") => FinishReason::ToolUse,
            Some("content_filter") => FinishReason::ContentFilter,
            Some(_) => FinishReason::Other,
        };

        let usage = resp
            .usage
            .map(|u| Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
                ..Default::default()
            })
            .unwrap_or_default();

        Ok(ChatResponse {
            model: resp.model.unwrap_or_else(|| fallback_model.to_string()),
            content: content_blocks,
            tool_calls,
            finish_reason,
            usage,
        })
    }

    /// Map a non-2xx HTTP response into the right [`LlmError`].
    pub(crate) fn classify_http_error(status: reqwest::StatusCode, body: &[u8]) -> LlmError {
        let body_text = String::from_utf8_lossy(body).to_string();
        let upstream_message = extract_error_message(&body_text);

        match status.as_u16() {
            401 | 403 => LlmError::Auth,
            429 => {
                let retry_after_ms = extract_retry_after_ms(&body_text).unwrap_or(1_000);
                LlmError::RateLimited { retry_after_ms }
            }
            _ => LlmError::Provider {
                status: status.as_u16(),
                message: upstream_message,
            },
        }
    }

    fn extract_error_message(body: &str) -> String {
        // OpenAI: `{"error":{"message":"...","type":"...","code":"..."}}`
        // DeepSeek / xAI / OpenRouter: similar shape.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
            if let Some(msg) = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
            {
                return crate::agent::llm::redact_body_for_error(msg);
            }
            if let Some(msg) = v.get("message").and_then(|m| m.as_str()) {
                return crate::agent::llm::redact_body_for_error(msg);
            }
        }
        // SECURITY: error bodies routinely echo prompts + key
        // fragments. Run them through the bearer / API-key
        // masking helper before surfacing.
        crate::agent::llm::redact_body_for_error(body)
    }

    fn extract_retry_after_ms(body: &str) -> Option<u64> {
        // OpenAI returns "Please try again in 1.234s" or similar. Best-effort.
        let s = body.to_lowercase();
        let after = s.split("try again in ").nth(1)?;
        let num: String = after
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let secs: f64 = num.parse().ok()?;
        Some((secs * 1000.0) as u64)
    }

    // -------------------------------------------------------------------
    // Streaming
    // -------------------------------------------------------------------
    //
    // OpenAI's `stream=true` shape:
    //
    //   data: {"choices":[{"delta":{"content":"hi"},"index":0,"finish_reason":null}]}
    //   data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"name":"f","arguments":""}}]}}]}
    //   data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\""}}]}}]}
    //   ...
    //   data: [DONE]
    //
    // Tool-call args arrive incrementally; we buffer them per index
    // and emit a single `ToolUse` event with the parsed JSON when
    // the stream finishes (or, on `[DONE]`, attempt a final parse).

    #[derive(Debug, Deserialize)]
    struct StreamChunk {
        #[serde(default)]
        choices: Vec<StreamChoice>,
        #[serde(default)]
        usage: Option<UsageJson>,
        #[serde(default)]
        model: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    struct StreamChoice {
        #[serde(default)]
        delta: StreamDelta,
        #[serde(default)]
        finish_reason: Option<String>,
    }

    #[derive(Debug, Default, Deserialize)]
    struct StreamDelta {
        #[serde(default)]
        content: Option<String>,
        #[serde(default)]
        tool_calls: Vec<StreamToolCall>,
    }

    #[derive(Debug, Deserialize)]
    struct StreamToolCall {
        #[serde(default)]
        index: usize,
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        function: Option<StreamToolFunction>,
    }

    #[derive(Debug, Deserialize)]
    struct StreamToolFunction {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        arguments: Option<String>,
    }

    struct PartialToolCall {
        id: String,
        name: String,
        args_buf: String,
        started: bool,
    }

    pub(crate) struct OpenAiStreamConverter {
        model: String,
        usage: Usage,
        finish: FinishReason,
        tool_calls: std::collections::BTreeMap<usize, PartialToolCall>,
        finished: bool,
    }

    impl OpenAiStreamConverter {
        pub(crate) fn new(model: String) -> Self {
            Self {
                model,
                usage: Usage::default(),
                finish: FinishReason::Stop,
                tool_calls: std::collections::BTreeMap::new(),
                finished: false,
            }
        }

        pub(crate) fn is_finished(&self) -> bool {
            self.finished
        }

        /// Process a single SSE event. Returns the StreamEvents to
        /// surface to the caller, in order.
        pub(crate) fn process(
            &mut self,
            sse: &crate::agent::llm::sse::SseEvent,
        ) -> Vec<Result<StreamEvent>> {
            if self.finished {
                return Vec::new();
            }
            let data = sse.data.trim();
            if data.is_empty() {
                return Vec::new();
            }
            if data == "[DONE]" {
                return self.finish_stream();
            }
            let chunk: StreamChunk = match serde_json::from_str(data) {
                Ok(c) => c,
                Err(e) => {
                    self.finished = true;
                    return vec![Err(LlmError::UpstreamMalformed(format!(
                        "openai stream chunk: {e}"
                    )))];
                }
            };

            if let Some(m) = chunk.model {
                self.model = m;
            }
            if let Some(u) = chunk.usage {
                self.usage = Usage {
                    input_tokens: u.prompt_tokens,
                    output_tokens: u.completion_tokens,
                    ..Default::default()
                };
            }

            let mut out: Vec<Result<StreamEvent>> = Vec::new();
            for ch in chunk.choices {
                if let Some(text) = ch.delta.content {
                    if !text.is_empty() {
                        out.push(Ok(StreamEvent::TextDelta { text }));
                    }
                }
                for tc in ch.delta.tool_calls {
                    let slot = self
                        .tool_calls
                        .entry(tc.index)
                        .or_insert_with(|| PartialToolCall {
                            id: String::new(),
                            name: String::new(),
                            args_buf: String::new(),
                            started: false,
                        });
                    if let Some(id) = tc.id {
                        slot.id = id;
                    }
                    if let Some(f) = tc.function {
                        if let Some(n) = f.name {
                            if !n.is_empty() {
                                slot.name = n;
                            }
                        }
                        if let Some(args) = f.arguments {
                            // Emit a single ToolUseStart on first
                            // delta for this index, then stream args
                            // as ToolInputDelta. We tolerate the
                            // case where `id` arrives in a later
                            // chunk by using a synthesised id until
                            // it lands.
                            if !slot.started && (!slot.name.is_empty() || !slot.id.is_empty()) {
                                let id = if slot.id.is_empty() {
                                    format!("tool_{}", tc.index)
                                } else {
                                    slot.id.clone()
                                };
                                let name = slot.name.clone();
                                out.push(Ok(StreamEvent::ToolUseStart { id, name }));
                                slot.started = true;
                            }
                            if !args.is_empty() {
                                slot.args_buf.push_str(&args);
                                if slot.started {
                                    out.push(Ok(StreamEvent::ToolInputDelta {
                                        id: if slot.id.is_empty() {
                                            format!("tool_{}", tc.index)
                                        } else {
                                            slot.id.clone()
                                        },
                                        partial_json: args,
                                    }));
                                }
                            }
                        }
                    }
                }
                if let Some(fr) = ch.finish_reason {
                    self.finish = match fr.as_str() {
                        "stop" | "end_turn" => FinishReason::Stop,
                        "length" | "max_tokens" => FinishReason::Length,
                        "tool_calls" | "function_call" => FinishReason::ToolUse,
                        "content_filter" => FinishReason::ContentFilter,
                        _ => FinishReason::Other,
                    };
                }
            }
            out
        }

        /// Flush buffered tool calls and emit the terminal `Done`
        /// event. Idempotent — repeated invocations are no-ops once
        /// finished.
        pub(crate) fn finish_stream(&mut self) -> Vec<Result<StreamEvent>> {
            if self.finished {
                return Vec::new();
            }
            self.finished = true;
            let mut out: Vec<Result<StreamEvent>> = Vec::new();
            // Flush any unstarted tool calls (no name yet → impossible
            // but be defensive) and accumulate parsed args into a
            // ToolUse event each.
            let calls = std::mem::take(&mut self.tool_calls);
            for (idx, slot) in calls {
                let id = if slot.id.is_empty() {
                    format!("tool_{idx}")
                } else {
                    slot.id
                };
                let input: serde_json::Value = if slot.args_buf.trim().is_empty() {
                    serde_json::Value::Object(serde_json::Map::new())
                } else {
                    match serde_json::from_str(&slot.args_buf) {
                        Ok(v) => v,
                        Err(e) => {
                            out.push(Err(LlmError::UpstreamMalformed(format!(
                                "tool_calls[{name}].arguments: {e}",
                                name = slot.name
                            ))));
                            continue;
                        }
                    }
                };
                if !slot.started {
                    out.push(Ok(StreamEvent::ToolUseStart {
                        id: id.clone(),
                        name: slot.name.clone(),
                    }));
                }
                out.push(Ok(StreamEvent::ToolUse(ToolCall {
                    id,
                    name: slot.name,
                    input,
                })));
            }
            out.push(Ok(StreamEvent::Done {
                finish: self.finish,
                usage: self.usage.clone(),
            }));
            out
        }
    }

    pub(crate) struct OpenAiStream {
        bytes: BoxStream<'static, std::result::Result<bytes::Bytes, reqwest::Error>>,
        parser: crate::agent::llm::sse::SseParser,
        converter: OpenAiStreamConverter,
        pending: std::collections::VecDeque<Result<StreamEvent>>,
        bytes_done: bool,
        total_bytes: usize,
        pool: Option<Arc<crate::agent::llm::credential_pool::Pool>>,
        lease: Option<crate::agent::llm::credential_pool::Lease>,
        accounted: bool,
    }

    impl OpenAiStream {
        pub(crate) fn new<S>(
            bytes: S,
            model: String,
            pool: Option<Arc<crate::agent::llm::credential_pool::Pool>>,
            lease: Option<crate::agent::llm::credential_pool::Lease>,
        ) -> Self
        where
            S: futures_util::Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>>
                + Send
                + 'static,
        {
            Self {
                bytes: bytes.boxed(),
                parser: crate::agent::llm::sse::SseParser::new(),
                converter: OpenAiStreamConverter::new(model),
                pending: std::collections::VecDeque::new(),
                bytes_done: false,
                total_bytes: 0,
                pool,
                lease,
                accounted: false,
            }
        }

        fn drain_parser(&mut self) {
            while let Some(sse) = self.parser.pop_event() {
                for ev in self.converter.process(&sse) {
                    self.pending.push_back(ev);
                }
            }
        }

        fn surface_overflow(&mut self, e: crate::agent::llm::sse::SseOverflow) {
            self.pending
                .push_back(Err(LlmError::UpstreamMalformed(format!(
                    "openai stream: {e}"
                ))));
            self.bytes_done = true;
            self.report_failure_once(crate::agent::llm::credential_pool::FailureClass::Transient);
        }

        fn report_failure_once(&mut self, cls: crate::agent::llm::credential_pool::FailureClass) {
            if self.accounted {
                return;
            }
            self.accounted = true;
            if let (Some(p), Some(l)) = (&self.pool, &self.lease) {
                p.report_failure(l, cls);
            }
        }

        fn report_success_once(&mut self) {
            if self.accounted {
                return;
            }
            self.accounted = true;
            if let (Some(p), Some(l)) = (&self.pool, &self.lease) {
                p.report_success(l);
            }
        }
    }

    impl futures_util::Stream for OpenAiStream {
        type Item = Result<StreamEvent>;

        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            use std::task::Poll;
            loop {
                if let Some(ev) = self.pending.pop_front() {
                    // Successful completion: when we surface the
                    // final `Done` event, credit the lease (MEDIUM-9
                    // success-on-DONE).
                    if matches!(ev, Ok(StreamEvent::Done { .. })) {
                        self.report_success_once();
                    } else if ev.is_err() && !self.accounted {
                        self.report_failure_once(
                            crate::agent::llm::credential_pool::FailureClass::Transient,
                        );
                    }
                    return Poll::Ready(Some(ev));
                }
                if self.bytes_done {
                    return Poll::Ready(None);
                }
                match std::pin::Pin::new(&mut self.bytes).poll_next(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => {
                        if let Err(e) = self.parser.finish() {
                            self.surface_overflow(e);
                            continue;
                        }
                        self.drain_parser();
                        // Stream ended without an explicit [DONE].
                        // Synthesise a Done event so callers can
                        // close out cleanly.
                        if !self.converter.is_finished() {
                            for ev in self.converter.finish_stream() {
                                self.pending.push_back(ev);
                            }
                        }
                        self.bytes_done = true;
                        continue;
                    }
                    Poll::Ready(Some(Ok(chunk))) => {
                        self.total_bytes = self.total_bytes.saturating_add(chunk.len());
                        if self.total_bytes > crate::agent::llm::MAX_STREAM_TOTAL_BYTES {
                            self.pending
                                .push_back(Err(LlmError::UpstreamMalformed(format!(
                                    "openai stream exceeded {} bytes",
                                    crate::agent::llm::MAX_STREAM_TOTAL_BYTES
                                ))));
                            self.bytes_done = true;
                            self.report_failure_once(
                                crate::agent::llm::credential_pool::FailureClass::Transient,
                            );
                            continue;
                        }
                        if let Err(e) = self.parser.feed(&chunk) {
                            self.surface_overflow(e);
                            continue;
                        }
                        self.drain_parser();
                        if self.converter.is_finished() {
                            self.bytes_done = true;
                        }
                        continue;
                    }
                    Poll::Ready(Some(Err(e))) => {
                        self.pending.push_back(Err(LlmError::Transport(e)));
                        self.bytes_done = true;
                        self.report_failure_once(
                            crate::agent::llm::error_classifier::classify_network_error(),
                        );
                        continue;
                    }
                }
            }
        }
    }
}

// Free function so the registry can decide whether the alias is one we own.
pub fn is_alias(name: &str) -> bool {
    PROVIDER_ALIASES.contains(&name)
}

// Construction helper used by the registry. Returns Arc<dyn Provider>.
pub fn build_provider(alias: &str, model: &str, agent: &AgentConfig) -> Arc<dyn Provider> {
    Arc::new(OpenAICompatProvider::from_agent_config(alias, model, agent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{Message, Role, Tool};

    fn cfg() -> AgentConfig {
        AgentConfig::default()
    }

    fn req_text(text: &str) -> ChatRequest {
        ChatRequest {
            model: "gpt-4o-mini".into(),
            messages: vec![Message::user_text(text)],
            system: Some("you are helpful".into()),
            tools: vec![],
            tool_choice: ToolChoice::default(),
            max_tokens: Some(64),
            temperature: Some(0.5),
            top_p: None,
            stop_sequences: vec![],
            extra: serde_json::Value::Null,
        }
    }

    // ---- alias / base URL resolution -------------------------------------

    #[test]
    fn default_base_urls_per_alias() {
        assert!(default_base_url_for("openai").starts_with("https://api.openai.com"));
        assert!(default_base_url_for("xai").starts_with("https://api.x.ai"));
        assert!(default_base_url_for("deepseek").starts_with("https://api.deepseek.com"));
        assert!(default_base_url_for("openrouter").starts_with("https://openrouter.ai"));
        assert!(default_base_url_for("ollama").contains("localhost:11434"));
        // Azure has no universal default — we return empty so the
        // wizard/apply layer can refuse the apply with a clear error.
        assert_eq!(default_base_url_for("azure"), "");
        assert!(default_base_url_for("__unknown__").starts_with("https://api.openai.com"));
    }

    #[test]
    fn azure_is_registered_alias() {
        assert!(is_alias("azure"));
        assert!(PROVIDER_ALIASES.contains(&"azure"));
    }

    #[test]
    fn azure_uses_api_key_header() {
        assert!(alias_uses_api_key_header("azure"));
        assert!(!alias_uses_api_key_header("openai"));
        assert!(!alias_uses_api_key_header("xai"));
    }

    #[test]
    fn azure_provider_not_configured_without_base_url() {
        let mut c = cfg();
        c.api_key_env = Some("DOES_NOT_EXIST_AZURE_KEY".into());
        // base_url not set → falls back to default_base_url_for("azure") = ""
        let provider = OpenAICompatProvider::from_agent_config("azure", "my-deployment", &c);
        assert!(!provider.is_configured());
    }

    #[tokio::test]
    async fn azure_chat_rejects_missing_base_url() {
        let c = cfg();
        let provider = OpenAICompatProvider::from_agent_config("azure", "my-deployment", &c);
        let err = provider.chat(req_text("hi")).await.unwrap_err();
        match err {
            LlmError::NotConfigured(msg) => {
                assert!(msg.contains("azure"), "msg: {msg}");
                assert!(msg.contains("base_url"), "msg: {msg}");
            }
            other => panic!("expected NotConfigured, got {other:?}"),
        }
    }

    #[test]
    fn config_uses_override_when_set() {
        let mut c = cfg();
        c.base_url = Some("https://my.proxy/v1".into());
        let oc = OpenAICompatConfig::from_agent_config("openai", "gpt-4o-mini", &c);
        assert_eq!(oc.base_url, "https://my.proxy/v1");
    }

    #[test]
    fn config_strips_trailing_slash() {
        let mut c = cfg();
        c.base_url = Some("https://my.proxy/v1/".into());
        let oc = OpenAICompatConfig::from_agent_config("openai", "gpt-4o-mini", &c);
        assert_eq!(oc.base_url, "https://my.proxy/v1");
    }

    #[test]
    fn empty_base_url_falls_back_to_alias_default() {
        let mut c = cfg();
        c.base_url = Some(String::new());
        let oc = OpenAICompatConfig::from_agent_config("xai", "grok", &c);
        assert!(oc.base_url.starts_with("https://api.x.ai"));
    }

    #[test]
    fn endpoint_handles_query_string_in_base_url() {
        // Non-azure alias: query string passthrough (e.g. a proxy that
        // requires a routing query). The path is appended in front of
        // the existing query.
        let mut c = cfg();
        c.base_url = Some("https://my.proxy.example.com/v1?route=blue".into());
        let provider = OpenAICompatProvider::from_agent_config("openai", "gpt-4o-mini", &c);
        assert_eq!(
            provider.endpoint(),
            "https://my.proxy.example.com/v1/chat/completions?route=blue"
        );
    }

    #[test]
    fn azure_endpoint_uses_resource_root_and_deployment_name() {
        // The user pastes the resource root from the Azure portal
        // (the same string the Python SDK takes as `azure_endpoint`)
        // and supplies the deployment name via `model`. The provider
        // composes the full `/openai/deployments/<dep>/chat/completions`
        // path itself, mirroring the official SDK behaviour.
        let mut c = cfg();
        c.base_url =
            Some("https://xiaoyu-eastus2.openai.azure.com/?api-version=2024-12-01-preview".into());
        let provider = OpenAICompatProvider::from_agent_config("azure", "gpt-5.4", &c);
        assert_eq!(
            provider.endpoint(),
            "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/gpt-5.4/chat/completions?api-version=2024-12-01-preview"
        );
    }

    #[test]
    fn azure_endpoint_strips_trailing_slash_on_resource_root() {
        let mut c = cfg();
        // Same resource root the user pasted in the wizard, no
        // trailing query.
        c.base_url = Some("https://acme.openai.azure.com/".into());
        let provider = OpenAICompatProvider::from_agent_config("azure", "my-deployment", &c);
        assert_eq!(
            provider.endpoint(),
            "https://acme.openai.azure.com/openai/deployments/my-deployment/chat/completions"
        );
    }

    #[test]
    fn azure_endpoint_handles_resource_root_without_trailing_slash() {
        let mut c = cfg();
        c.base_url = Some("https://acme.openai.azure.com?api-version=2024-12-01-preview".into());
        let provider = OpenAICompatProvider::from_agent_config("azure", "gpt-5.4", &c);
        assert_eq!(
            provider.endpoint(),
            "https://acme.openai.azure.com/openai/deployments/gpt-5.4/chat/completions?api-version=2024-12-01-preview"
        );
    }

    #[test]
    fn endpoint_appends_path_when_no_query_string() {
        let mut c = cfg();
        c.base_url = Some("https://api.openai.com/v1".into());
        let provider = OpenAICompatProvider::from_agent_config("openai", "gpt-4o-mini", &c);
        assert_eq!(
            provider.endpoint(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn responses_endpoint_preserves_query_string() {
        assert_eq!(
            endpoint_for_wire_api(
                "https://api.individual.githubcopilot.com?region=west",
                crate::agent::llm::providers::copilot_auth::CopilotWireApi::Responses,
            ),
            "https://api.individual.githubcopilot.com/responses?region=west"
        );
    }

    #[test]
    fn copilot_headers_include_responses_api_contract() {
        let request = with_copilot_headers(
            reqwest::Client::new().post("https://api.individual.githubcopilot.com/responses"),
            true,
            crate::agent::llm::providers::copilot_auth::COPILOT_INITIATOR_USER,
        )
        .build()
        .unwrap();
        let headers = request.headers();
        assert_eq!(
            headers["X-GitHub-Api-Version"],
            crate::agent::llm::providers::copilot_auth::GITHUB_API_VERSION
        );
        assert_eq!(
            headers["X-Initiator"],
            crate::agent::llm::providers::copilot_auth::COPILOT_INITIATOR_USER
        );
        assert_eq!(
            headers["X-Interaction-Type"],
            crate::agent::llm::providers::copilot_auth::COPILOT_INTERACTION_TYPE
        );
        assert_eq!(
            headers["OpenAI-Intent"],
            crate::agent::llm::providers::copilot_auth::COPILOT_INTERACTION_TYPE
        );
        assert_eq!(headers["Copilot-Vision-Request"], "true");
    }

    #[test]
    fn copilot_initiator_distinguishes_user_and_tool_follow_up() {
        let user_request = req_text("hello");
        assert_eq!(
            copilot_initiator(&user_request),
            crate::agent::llm::providers::copilot_auth::COPILOT_INITIATOR_USER
        );

        let mut automatic_request = req_text("summarise");
        automatic_request.extra = serde_json::json!({"_cos_initiator": "agent"});
        assert_eq!(
            copilot_initiator(&automatic_request),
            crate::agent::llm::providers::copilot_auth::COPILOT_INITIATOR_AGENT
        );

        let mut tool_request = req_text("unused");
        tool_request.messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                is_error: false,
                content: "done".into(),
            }],
        }];
        assert_eq!(
            copilot_initiator(&tool_request),
            crate::agent::llm::providers::copilot_auth::COPILOT_INITIATOR_AGENT
        );
    }

    // ---- credential / env resolution -------------------------------------

    #[test]
    fn resolve_api_key_returns_none_when_neither_source_set() {
        // Cred name None, env name None.
        assert_eq!(resolve_api_key(None, None).unwrap(), None);
    }

    #[test]
    fn resolve_api_key_uses_env_when_credential_missing() {
        std::env::set_var("COS_TEST_KEY_VAR_8742", "sk-from-env");
        let v = resolve_api_key(None, Some("COS_TEST_KEY_VAR_8742")).unwrap();
        assert_eq!(v.as_deref(), Some("sk-from-env"));
        std::env::remove_var("COS_TEST_KEY_VAR_8742");
    }

    #[test]
    fn resolve_api_key_ignores_empty_env() {
        std::env::set_var("COS_TEST_KEY_VAR_8743", "");
        let v = resolve_api_key(None, Some("COS_TEST_KEY_VAR_8743")).unwrap();
        assert_eq!(v, None);
        std::env::remove_var("COS_TEST_KEY_VAR_8743");
    }

    // ---- is_configured ---------------------------------------------------

    #[test]
    fn is_configured_true_when_api_key_present() {
        let mut c = cfg();
        c.api_key_env = Some("COS_TEST_KEY_PRESENT_X".into());
        std::env::set_var("COS_TEST_KEY_PRESENT_X", "sk-x");
        let p = OpenAICompatProvider::from_agent_config("openai", "gpt-4o-mini", &c);
        assert!(p.is_configured());
        std::env::remove_var("COS_TEST_KEY_PRESENT_X");
    }

    #[test]
    fn is_configured_false_for_openai_without_key() {
        let p = OpenAICompatProvider::from_agent_config("openai", "gpt-4o-mini", &cfg());
        assert!(!p.is_configured());
    }

    #[test]
    fn is_configured_true_for_ollama_without_key() {
        // Local default — no API key required.
        let p = OpenAICompatProvider::from_agent_config("ollama", "llama3.2:3b", &cfg());
        assert!(p.is_configured());
    }

    // ---- request body serialisation --------------------------------------

    #[test]
    fn builds_minimal_chat_body() {
        let r = req_text("hello");
        let body = wire::build_request_body(&r, "gpt-4o-mini", false);
        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "you are helpful");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "hello");
        assert_eq!(body["max_tokens"], 64);
        assert!(body.get("tools").is_none(), "no tools means no tools field");
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn builds_responses_body_with_tool_history() {
        let mut request = req_text("unused");
        request.messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Reasoning {
                        id: "rs_42".into(),
                        summary: vec!["Need to inspect the requested file.".into()],
                        encrypted_content: Some("opaque-ciphertext".into()),
                    },
                    ContentBlock::Text {
                        text: "I'll inspect it.".into(),
                    },
                    ContentBlock::ToolState {
                        tool_use_id: "call_42".into(),
                        thought_signature: "opaque-thought-signature".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "call_42".into(),
                        name: "read_file".into(),
                        input: serde_json::json!({"path": "/tmp/a"}),
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_42".into(),
                    is_error: false,
                    content: "hello".into(),
                }],
            },
        ];
        request.tools = vec![Tool {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        }];
        request.tool_choice = ToolChoice::Tool {
            name: "read_file".into(),
        };
        request.extra = serde_json::json!({
            "store": true,
            "include": ["not.allowed"],
            "seed": 42,
            "_cos_initiator": "agent"
        });

        let body = responses_wire::build_request_body(&request, "gpt-5.6-sol", true);
        assert_eq!(body["model"], "gpt-5.6-sol");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert_eq!(body["seed"], 42);
        assert!(body.get("_cos_initiator").is_none());
        assert_eq!(body["max_output_tokens"], 64);
        assert!(body.get("temperature").is_none());
        assert_eq!(body["input"][0]["type"], "message");
        assert_eq!(body["input"][0]["role"], "system");
        assert_eq!(body["input"][1]["type"], "reasoning");
        assert_eq!(body["input"][1]["id"], "rs_42");
        assert_eq!(body["input"][1]["encrypted_content"], "opaque-ciphertext");
        assert_eq!(body["input"][2]["content"][0]["type"], "output_text");
        assert_eq!(body["input"][3]["type"], "function_call");
        assert_eq!(body["input"][3]["call_id"], "call_42");
        assert_eq!(
            body["input"][3]["thought_signature"],
            "opaque-thought-signature"
        );
        assert_eq!(body["input"][4]["type"], "function_call_output");
        assert_eq!(body["input"][4]["output"], "hello");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "read_file");
        assert!(body["tools"][0].get("function").is_none());
        assert_eq!(body["tool_choice"]["type"], "function");
        assert_eq!(body["tool_choice"]["name"], "read_file");
    }

    #[test]
    fn responses_does_not_replay_reasoning_without_encrypted_state() {
        let mut request = req_text("unused");
        request.system = None;
        request.messages = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Reasoning {
                id: "rs_without_ciphertext".into(),
                summary: vec!["Visible summary only".into()],
                encrypted_content: None,
            }],
        }];
        let body = responses_wire::build_request_body(&request, "gpt-5.6-sol", false);
        assert!(
            body["input"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["type"] != "reasoning"),
            "reasoning without encrypted_content must not be replayed"
        );
    }

    #[test]
    fn modern_models_use_max_completion_tokens() {
        for m in &[
            "gpt-5",
            "gpt-5.4-mini",
            "gpt-6-pro",
            "o1-mini",
            "o3",
            "o4-preview",
        ] {
            assert!(
                wire::use_max_completion_tokens(m),
                "expected {m} to use max_completion_tokens"
            );
        }
        for m in &[
            "gpt-4o-mini",
            "gpt-4.1",
            "gpt-3.5-turbo",
            "claude-3.5-sonnet",
            "llama3.2:3b",
            "deepseek-chat",
        ] {
            assert!(
                !wire::use_max_completion_tokens(m),
                "expected {m} to use legacy max_tokens"
            );
        }
    }

    #[test]
    fn body_uses_max_completion_tokens_for_gpt5() {
        let r = req_text("hi");
        let body = wire::build_request_body(&r, "gpt-5.4-mini", false);
        assert_eq!(body["max_completion_tokens"], 64);
        assert!(
            body.get("max_tokens").is_none(),
            "legacy field must be absent"
        );
        // o-series / gpt-5 only support default temperature → field omitted.
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn body_includes_tools_when_provided() {
        let mut r = req_text("call tool");
        r.tools = vec![Tool {
            name: "echo".into(),
            description: "echo it".into(),
            input_schema: serde_json::json!({"type":"object","properties":{}}),
        }];
        let body = wire::build_request_body(&r, "gpt-4o-mini", false);
        assert_eq!(body["tools"][0]["function"]["name"], "echo");
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn body_marks_stream_when_requested() {
        let r = req_text("hi");
        let body = wire::build_request_body(&r, "m", true);
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn body_renders_assistant_tool_use() {
        let mut r = req_text("ignored");
        r.messages.push(crate::agent::llm::Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "echo".into(),
                input: serde_json::json!({"text":"hi"}),
            }],
        });
        let body = wire::build_request_body(&r, "m", false);
        let asst = &body["messages"][2];
        assert_eq!(asst["role"], "assistant");
        assert_eq!(asst["tool_calls"][0]["id"], "call_1");
        assert_eq!(asst["tool_calls"][0]["function"]["name"], "echo");
        let args = asst["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap();
        assert!(args.contains("hi"));
    }

    #[test]
    fn body_renders_tool_result_as_tool_role() {
        let mut r = req_text("ignored");
        r.messages.push(crate::agent::llm::Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                is_error: false,
                content: "{\"ok\":true}".into(),
            }],
        });
        let body = wire::build_request_body(&r, "m", false);
        let tool_msg = &body["messages"][2];
        assert_eq!(tool_msg["role"], "tool");
        assert_eq!(tool_msg["tool_call_id"], "call_1");
        assert_eq!(tool_msg["content"], "{\"ok\":true}");
    }

    #[test]
    fn body_fans_out_multiple_tool_results_into_separate_tool_messages() {
        // Regression for Azure 400 "tool_call_ids did not have
        // response messages" when the assistant calls multiple
        // tools in one turn. The runtime aggregates all tool
        // results into a single User message containing several
        // ToolResult blocks; the wire serializer must emit each
        // one as its own `role=tool` message with the matching
        // tool_call_id, otherwise the conversation history is
        // malformed.
        let mut r = req_text("inventory");
        r.messages.push(crate::agent::llm::Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: "call_A".into(),
                    name: "mounts".into(),
                    input: serde_json::json!({}),
                },
                ContentBlock::ToolUse {
                    id: "call_B".into(),
                    name: "recent".into(),
                    input: serde_json::json!({"limit": 50}),
                },
            ],
        });
        r.messages.push(crate::agent::llm::Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_A".into(),
                    is_error: false,
                    content: "{\"mounts\":[]}".into(),
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_B".into(),
                    is_error: false,
                    content: "{\"files\":[]}".into(),
                },
            ],
        });
        let body = wire::build_request_body(&r, "m", false);
        let msgs = body["messages"].as_array().expect("messages array");
        // system + user "inventory" + assistant with two tool_calls
        // + two role=tool messages = 5 total.
        assert_eq!(msgs.len(), 5, "got: {msgs:?}");
        let tool_a = &msgs[3];
        let tool_b = &msgs[4];
        assert_eq!(tool_a["role"], "tool");
        assert_eq!(tool_a["tool_call_id"], "call_A");
        assert_eq!(tool_a["content"], "{\"mounts\":[]}");
        assert_eq!(tool_b["role"], "tool");
        assert_eq!(tool_b["tool_call_id"], "call_B");
        assert_eq!(tool_b["content"], "{\"files\":[]}");
    }

    #[test]
    fn body_merges_extras() {
        let mut r = req_text("hi");
        r.extra = serde_json::json!({"seed": 42, "response_format": {"type":"json_object"}});
        let body = wire::build_request_body(&r, "m", false);
        assert_eq!(body["seed"], 42);
        assert_eq!(body["response_format"]["type"], "json_object");
    }

    // ---- response parsing ------------------------------------------------

    #[test]
    fn parses_responses_text_tool_and_usage() {
        let raw = br#"{
            "model": "gpt-5.6-sol",
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "Checking."}]
                },
                {
                    "type": "reasoning",
                    "id": "rs_1",
                    "summary": [{
                        "type": "summary_text",
                        "text": "Inspect the file before answering."
                    }],
                    "encrypted_content": "opaque-ciphertext"
                },
                {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"/tmp/a\"}",
                    "thought_signature": "opaque-thought-signature"
                }
            ],
            "usage": {
                "input_tokens": 12,
                "output_tokens": 7,
                "input_tokens_details": {"cached_tokens": 4}
            }
        }"#;
        let chat = responses_wire::response_from_slice(raw, "fallback").unwrap();
        assert_eq!(chat.model, "gpt-5.6-sol");
        assert_eq!(chat.finish_reason, FinishReason::ToolUse);
        assert_eq!(chat.tool_calls.len(), 1);
        assert_eq!(chat.tool_calls[0].id, "call_1");
        assert_eq!(chat.tool_calls[0].name, "read_file");
        assert_eq!(chat.tool_calls[0].input["path"], "/tmp/a");
        assert_eq!(chat.usage.input_tokens, 12);
        assert_eq!(chat.usage.output_tokens, 7);
        assert_eq!(chat.usage.cache_read_tokens, 4);
        assert!(matches!(
            &chat.content[0],
            ContentBlock::Text { text } if text == "Checking."
        ));
        assert!(matches!(
            &chat.content[1],
            ContentBlock::Reasoning {
                id,
                encrypted_content: Some(encrypted),
                ..
            } if id == "rs_1" && encrypted == "opaque-ciphertext"
        ));
        assert!(matches!(
            &chat.content[2],
            ContentBlock::ToolState {
                tool_use_id,
                thought_signature,
            } if tool_use_id == "call_1" && thought_signature == "opaque-thought-signature"
        ));
        assert!(matches!(
            &chat.content[3],
            ContentBlock::ToolUse { id, name, .. }
                if id == "call_1" && name == "read_file"
        ));
    }

    #[test]
    fn parses_responses_incomplete_and_refusal() {
        let incomplete = br#"{
            "status": "incomplete",
            "output": [],
            "incomplete_details": {"reason": "max_output_tokens"}
        }"#;
        let chat = responses_wire::response_from_slice(incomplete, "m").unwrap();
        assert_eq!(chat.finish_reason, FinishReason::Length);

        let refusal = br#"{
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{"type": "refusal", "refusal": "Cannot comply."}]
            }]
        }"#;
        let chat = responses_wire::response_from_slice(refusal, "m").unwrap();
        assert_eq!(chat.finish_reason, FinishReason::Refusal);
        assert!(matches!(
            &chat.content[0],
            ContentBlock::Text { text } if text == "Cannot comply."
        ));
    }

    #[test]
    fn responses_error_codes_preserve_retry_semantics() {
        let rate_limited = br#"{
            "status": "failed",
            "error": {
                "code": "rate_limit_exceeded",
                "message": "too many requests"
            },
            "output": []
        }"#;
        assert!(matches!(
            responses_wire::response_from_slice(rate_limited, "m"),
            Err(LlmError::RateLimited { .. })
        ));

        let server_error = br#"{
            "status": "failed",
            "error": {
                "code": "server_error",
                "message": "temporary failure"
            },
            "output": []
        }"#;
        assert!(matches!(
            responses_wire::response_from_slice(server_error, "m"),
            Err(LlmError::Provider { status: 500, .. })
        ));
    }

    #[test]
    fn parses_simple_text_response() {
        let raw = r#"{
            "id":"x","object":"chat.completion","created":1,
            "model":"gpt-4o-mini",
            "choices":[{"index":0,"finish_reason":"stop",
                "message":{"role":"assistant","content":"hi there"}}],
            "usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}
        }"#;
        let resp: wire::Response = serde_json::from_str(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        assert_eq!(chat.model, "gpt-4o-mini");
        assert_eq!(chat.finish_reason, FinishReason::Stop);
        match &chat.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hi there"),
            _ => panic!("expected text block"),
        }
        assert_eq!(chat.usage.input_tokens, 5);
        assert_eq!(chat.usage.output_tokens, 2);
    }

    #[test]
    fn parses_tool_use_response() {
        let raw = r#"{
            "model":"gpt-4o-mini",
            "choices":[{"index":0,"finish_reason":"tool_calls",
                "message":{"role":"assistant","content":null,"tool_calls":[
                    {"id":"call_42","type":"function",
                     "function":{"name":"echo","arguments":"{\"text\":\"hi\"}"}}
                ]}}]
        }"#;
        let resp: wire::Response = serde_json::from_str(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        assert_eq!(chat.finish_reason, FinishReason::ToolUse);
        assert_eq!(chat.tool_calls.len(), 1);
        assert_eq!(chat.tool_calls[0].id, "call_42");
        assert_eq!(chat.tool_calls[0].name, "echo");
        assert_eq!(chat.tool_calls[0].input["text"], "hi");
    }

    #[test]
    fn parses_length_finish_as_length() {
        let raw = r#"{"choices":[{"finish_reason":"length",
            "message":{"role":"assistant","content":"truncated..."}}]}"#;
        let resp: wire::Response = serde_json::from_str(raw).unwrap();
        let chat = wire::response_to_chat(resp, "m").unwrap();
        assert_eq!(chat.finish_reason, FinishReason::Length);
    }

    #[test]
    fn parses_response_without_usage() {
        let raw = r#"{"choices":[{"finish_reason":"stop",
            "message":{"role":"assistant","content":"ok"}}]}"#;
        let resp: wire::Response = serde_json::from_str(raw).unwrap();
        let chat = wire::response_to_chat(resp, "m").unwrap();
        assert_eq!(chat.usage.input_tokens, 0);
    }

    #[test]
    fn parse_error_when_no_choices() {
        let raw = r#"{"choices":[]}"#;
        let resp: wire::Response = serde_json::from_str(raw).unwrap();
        let err = wire::response_to_chat(resp, "m").unwrap_err();
        match err {
            LlmError::Parse(_) => {}
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    // ---- error classification --------------------------------------------

    #[test]
    fn classifies_401_as_auth() {
        let err = wire::classify_http_error(
            reqwest::StatusCode::from_u16(401).unwrap(),
            br#"{"error":{"message":"Bad key"}}"#,
        );
        assert!(matches!(err, LlmError::Auth));
    }

    #[test]
    fn classifies_403_as_auth() {
        let err =
            wire::classify_http_error(reqwest::StatusCode::from_u16(403).unwrap(), b"forbidden");
        assert!(matches!(err, LlmError::Auth));
    }

    #[test]
    fn classifies_429_as_rate_limited_with_retry_after() {
        let body = br#"{"error":{"message":"Rate limit. Please try again in 0.5s."}}"#;
        let err = wire::classify_http_error(reqwest::StatusCode::from_u16(429).unwrap(), body);
        match err {
            LlmError::RateLimited { retry_after_ms } => assert_eq!(retry_after_ms, 500),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn classifies_500_as_provider_with_message() {
        let body = br#"{"error":{"message":"upstream borked"}}"#;
        let err = wire::classify_http_error(reqwest::StatusCode::from_u16(500).unwrap(), body);
        match err {
            LlmError::Provider { status, message } => {
                assert_eq!(status, 500);
                assert_eq!(message, "upstream borked");
            }
            other => panic!("expected Provider, got {other:?}"),
        }
    }

    // ---- end-to-end against an inline TCP mock --------------------------

    /// Tiny inline HTTP/1.1 mock that accepts one connection, reads the
    /// request, and sends back a fixed response. Returns the bound URL
    /// and a join handle that yields the request body bytes. Avoids
    /// pulling in `wiremock` / `mockito` as dev-deps.
    async fn spawn_one_shot_mock(
        status_line: &'static str,
        response_body: &'static str,
    ) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/v1");

        let body_bytes = response_body.as_bytes().to_vec();
        let resp = format!(
            "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body_bytes.len()
        );

        let handle = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut socket, _) = listener.accept().await.unwrap();
            // Read until headers end.
            let mut buf = Vec::with_capacity(4096);
            let mut tmp = [0u8; 4096];
            let mut header_end = None;
            let mut content_length: usize = 0;
            loop {
                let n = socket.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if header_end.is_none() {
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                        // Parse Content-Length.
                        let headers = std::str::from_utf8(&buf[..pos]).unwrap_or("");
                        for line in headers.split("\r\n") {
                            if let Some(rest) =
                                line.to_ascii_lowercase().strip_prefix("content-length:")
                            {
                                content_length = rest.trim().parse().unwrap_or(0);
                            }
                        }
                    }
                }
                if let Some(start) = header_end {
                    if buf.len() - start >= content_length {
                        break;
                    }
                }
            }
            let _ = socket.write_all(resp.as_bytes()).await;
            let _ = socket.write_all(&body_bytes).await;
            let _ = socket.shutdown().await;
            buf
        });

        (url, handle)
    }

    #[tokio::test]
    async fn end_to_end_chat_round_trip_via_inline_mock() {
        let response_body = r#"{
            "id":"x","object":"chat.completion","created":1,
            "model":"gpt-4o-mini",
            "choices":[{"index":0,"finish_reason":"stop",
                "message":{"role":"assistant","content":"hi from mock"}}],
            "usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}
        }"#;
        let (base_url, handle) = spawn_one_shot_mock("HTTP/1.1 200 OK", response_body).await;

        let mut c = AgentConfig::default();
        c.base_url = Some(base_url.clone());
        c.api_key_env = Some("COS_TEST_E2E_KEY".into());
        c.request_timeout = 5;
        std::env::set_var("COS_TEST_E2E_KEY", "sk-test");

        let provider = OpenAICompatProvider::from_agent_config("openai", "gpt-4o-mini", &c);
        let req = req_text("hello");
        let resp = provider.chat(req).await.expect("chat should succeed");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        match &resp.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hi from mock"),
            _ => panic!("expected text"),
        }
        assert_eq!(resp.usage.input_tokens, 3);
        assert_eq!(resp.usage.output_tokens, 4);

        let request_bytes = handle.await.unwrap();
        let request = String::from_utf8_lossy(&request_bytes).to_lowercase();
        assert!(request.contains("post /v1/chat/completions"));
        assert!(request.contains("authorization: bearer sk-test"));
        assert!(request.contains("\"model\":\"gpt-4o-mini\""));
        assert!(request.contains("\"hello\""));

        std::env::remove_var("COS_TEST_E2E_KEY");
    }

    #[tokio::test]
    async fn end_to_end_401_maps_to_auth_error() {
        let body = r#"{"error":{"message":"bad key"}}"#;
        let (base_url, handle) = spawn_one_shot_mock("HTTP/1.1 401 Unauthorized", body).await;

        let mut c = AgentConfig::default();
        c.base_url = Some(base_url);
        c.api_key_env = Some("COS_TEST_BAD_KEY".into());
        c.request_timeout = 5;
        std::env::set_var("COS_TEST_BAD_KEY", "sk-bad");

        let provider = OpenAICompatProvider::from_agent_config("openai", "gpt-4o-mini", &c);
        let err = provider.chat(req_text("hi")).await.unwrap_err();
        assert!(matches!(err, LlmError::Auth), "got {err:?}");

        let _ = handle.await;
        std::env::remove_var("COS_TEST_BAD_KEY");
    }

    #[tokio::test]
    async fn azure_alias_sends_api_key_header_not_bearer() {
        let response_body = r#"{
            "id":"x","object":"chat.completion","created":1,
            "model":"my-deployment",
            "choices":[{"index":0,"finish_reason":"stop",
                "message":{"role":"assistant","content":"hi"}}],
            "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
        }"#;
        let (base_url, handle) = spawn_one_shot_mock("HTTP/1.1 200 OK", response_body).await;
        // base_url ends in `/v1` from the mock helper — for the
        // assertion that matters (which header is sent) the exact
        // path doesn't matter, just that the request goes out.

        let mut c = AgentConfig::default();
        c.base_url = Some(base_url);
        c.api_key_env = Some("COS_TEST_AZURE_KEY".into());
        c.request_timeout = 5;
        std::env::set_var("COS_TEST_AZURE_KEY", "az-secret-123");

        let provider = OpenAICompatProvider::from_agent_config("azure", "my-deployment", &c);
        let _ = provider.chat(req_text("hi")).await;

        let request_bytes = handle.await.unwrap();
        let request = String::from_utf8_lossy(&request_bytes);
        let lower = request.to_lowercase();
        assert!(
            lower.contains("api-key: az-secret-123"),
            "expected Azure api-key header, got headers:\n{}",
            request
        );
        assert!(
            !lower.contains("authorization: bearer"),
            "Azure should not send Authorization: Bearer, got headers:\n{}",
            request
        );

        std::env::remove_var("COS_TEST_AZURE_KEY");
    }

    #[tokio::test]
    async fn openai_alias_still_sends_bearer_not_api_key() {
        let response_body = r#"{
            "id":"x","object":"chat.completion","created":1,
            "model":"gpt-4o-mini",
            "choices":[{"index":0,"finish_reason":"stop",
                "message":{"role":"assistant","content":"hi"}}]
        }"#;
        let (base_url, handle) = spawn_one_shot_mock("HTTP/1.1 200 OK", response_body).await;

        let mut c = AgentConfig::default();
        c.base_url = Some(base_url);
        c.api_key_env = Some("COS_TEST_OPENAI_BEARER_KEY".into());
        c.request_timeout = 5;
        std::env::set_var("COS_TEST_OPENAI_BEARER_KEY", "sk-openai");

        let provider = OpenAICompatProvider::from_agent_config("openai", "gpt-4o-mini", &c);
        let _ = provider.chat(req_text("hi")).await;

        let request = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
        assert!(request.contains("authorization: bearer sk-openai"));
        assert!(!request.contains("api-key: "));

        std::env::remove_var("COS_TEST_OPENAI_BEARER_KEY");
    }

    #[tokio::test]
    async fn end_to_end_includes_extra_headers() {
        let response_body = r#"{"choices":[{"finish_reason":"stop",
            "message":{"role":"assistant","content":"ok"}}]}"#;
        let (base_url, handle) = spawn_one_shot_mock("HTTP/1.1 200 OK", response_body).await;

        let mut c = AgentConfig::default();
        c.base_url = Some(base_url);
        c.extra_headers
            .insert("HTTP-Referer".into(), "https://cos.example".into());
        c.extra_headers.insert("X-Title".into(), "cos agent".into());
        c.request_timeout = 5;

        let provider = OpenAICompatProvider::from_agent_config("openrouter", "openrouter/auto", &c);
        let _ = provider.chat(req_text("hi")).await; // success or not, we want to inspect req
        let request = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
        assert!(request.contains("http-referer: https://cos.example"));
        assert!(request.contains("x-title: cos agent"));
    }

    // ---- credential pool wiring ------------------------------------------

    #[test]
    fn no_pool_when_neither_plural_field_set() {
        let c = AgentConfig::default();
        let oc = OpenAICompatConfig::from_agent_config("openai", "gpt-4o-mini", &c);
        assert!(oc.pool.is_none());
    }

    #[test]
    fn pool_built_from_envs() {
        std::env::set_var("COS_TEST_POOL_KEY_A", "sk-aaa");
        std::env::set_var("COS_TEST_POOL_KEY_B", "sk-bbb");
        let mut c = AgentConfig::default();
        c.api_key_envs = vec!["COS_TEST_POOL_KEY_A".into(), "COS_TEST_POOL_KEY_B".into()];
        let oc = OpenAICompatConfig::from_agent_config("openai", "gpt-4o-mini", &c);
        std::env::remove_var("COS_TEST_POOL_KEY_A");
        std::env::remove_var("COS_TEST_POOL_KEY_B");
        let pool = oc.pool.expect("pool should be built");
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn pool_unresolved_falls_back_to_single_key_silently() {
        let mut c = AgentConfig::default();
        c.api_key_envs = vec!["COS_TEST_DOES_NOT_EXIST_ENV_AAAA".into()];
        let oc = OpenAICompatConfig::from_agent_config("openai", "gpt-4o-mini", &c);
        // Pool empty → warn-and-fall-through; single-key path stays
        // available (empty in this case since no api_key_credential
        // is set either).
        assert!(oc.pool.is_none());
        assert!(oc.api_key.is_none());
    }

    #[test]
    fn is_configured_true_with_pool_only() {
        std::env::set_var("COS_TEST_POOL_ICONFIG_X", "sk-x");
        let mut c = AgentConfig::default();
        c.api_key_envs = vec!["COS_TEST_POOL_ICONFIG_X".into()];
        let oc = OpenAICompatConfig::from_agent_config("openai", "gpt-4o-mini", &c);
        std::env::remove_var("COS_TEST_POOL_ICONFIG_X");
        let provider = OpenAICompatProvider::new(oc);
        assert!(provider.is_configured());
    }

    #[test]
    fn pool_strategy_round_robin_parsed() {
        std::env::set_var("COS_TEST_POOL_RR_X", "k1");
        std::env::set_var("COS_TEST_POOL_RR_Y", "k2");
        let mut c = AgentConfig::default();
        c.api_key_envs = vec!["COS_TEST_POOL_RR_X".into(), "COS_TEST_POOL_RR_Y".into()];
        c.pool_strategy = "round-robin".into();
        let oc = OpenAICompatConfig::from_agent_config("openai", "gpt-4o-mini", &c);
        std::env::remove_var("COS_TEST_POOL_RR_X");
        std::env::remove_var("COS_TEST_POOL_RR_Y");
        let pool = oc.pool.expect("pool should be built");
        assert_eq!(
            pool.strategy(),
            crate::agent::llm::credential_pool::SelectionStrategy::RoundRobin
        );
    }

    #[test]
    fn pool_cooldown_picked_up_from_config() {
        std::env::set_var("COS_TEST_POOL_CD_X", "k1");
        let mut c = AgentConfig::default();
        c.api_key_envs = vec!["COS_TEST_POOL_CD_X".into()];
        c.pool_cooldown_secs = 5;
        let oc = OpenAICompatConfig::from_agent_config("openai", "gpt-4o-mini", &c);
        std::env::remove_var("COS_TEST_POOL_CD_X");
        let pool = oc.pool.expect("pool should be built");
        assert_eq!(pool.cooldown(), std::time::Duration::from_secs(5));
    }

    #[tokio::test]
    async fn end_to_end_uses_pool_lease_as_bearer_token() {
        std::env::set_var("COS_TEST_POOL_LEASE_K", "sk-from-pool-aaa");
        let response_body = r#"{"choices":[{"finish_reason":"stop",
            "message":{"role":"assistant","content":"ok"}}]}"#;
        let (base_url, handle) = spawn_one_shot_mock("HTTP/1.1 200 OK", response_body).await;

        let mut c = AgentConfig::default();
        c.base_url = Some(base_url);
        c.api_key_envs = vec!["COS_TEST_POOL_LEASE_K".into()];
        c.request_timeout = 5;

        let provider = OpenAICompatProvider::from_agent_config("openai", "gpt-4o-mini", &c);
        let _ = provider.chat(req_text("hi")).await;
        std::env::remove_var("COS_TEST_POOL_LEASE_K");
        let request = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
        assert!(
            request.contains("authorization: bearer sk-from-pool-aaa"),
            "expected pool key in Authorization header, got:\n{request}"
        );
    }

    #[tokio::test]
    async fn end_to_end_pool_records_failure_on_401() {
        std::env::set_var("COS_TEST_POOL_FAIL_K", "sk-bad");
        let (base_url, handle) = spawn_one_shot_mock(
            "HTTP/1.1 401 Unauthorized",
            r#"{"error":{"message":"invalid api key"}}"#,
        )
        .await;

        let mut c = AgentConfig::default();
        c.base_url = Some(base_url);
        c.api_key_envs = vec!["COS_TEST_POOL_FAIL_K".into()];
        c.pool_cooldown_secs = 60;
        c.request_timeout = 5;

        let provider = OpenAICompatProvider::from_agent_config("openai", "gpt-4o-mini", &c);
        let pool_handle = provider.cfg.pool.clone().expect("pool built");
        assert_eq!(pool_handle.len(), 1);

        let err = provider.chat(req_text("hi")).await.unwrap_err();
        std::env::remove_var("COS_TEST_POOL_FAIL_K");
        let _ = handle.await;
        assert!(matches!(err, LlmError::Auth), "got {err:?}");

        let stats = pool_handle.stats();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].failures, 1);
    }

    #[test]
    fn responses_stream_emits_text_tool_and_terminal_usage() {
        use crate::agent::llm::sse::SseEvent;

        let event = |data: serde_json::Value| SseEvent {
            event: "message".into(),
            data: data.to_string(),
        };
        let mut converter = responses_wire::ResponsesStreamConverter::new("gpt-5.6-sol".into());

        let reasoning = converter.process(&event(serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "reasoning",
                "id": "rs_1",
                "summary": [{
                    "type": "summary_text",
                    "text": "Need to inspect the file."
                }],
                "encrypted_content": "opaque-ciphertext"
            }
        })));
        assert!(matches!(
            &reasoning[0],
            Ok(StreamEvent::Reasoning {
                id,
                encrypted_content: Some(content),
                ..
            }) if id == "rs_1" && content == "opaque-ciphertext"
        ));

        let text = converter.process(&event(serde_json::json!({
            "type": "response.output_text.delta",
            "item_id": "msg_1",
            "output_index": 0,
            "delta": "Hello"
        })));
        assert!(matches!(
            &text[0],
            Ok(StreamEvent::TextDelta { text }) if text == "Hello"
        ));

        let start = converter.process(&event(serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 1,
            "item": {
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "read_file",
                "arguments": "",
                "thought_signature": "opaque-thought-signature"
            }
        })));
        assert!(matches!(
            &start[0],
            Ok(StreamEvent::ToolUseStart { id, name })
                if id == "call_1" && name == "read_file"
        ));

        let delta = converter.process(&event(serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 1,
            "delta": "{\"path\":\"/tmp/a\"}"
        })));
        assert!(matches!(
            &delta[0],
            Ok(StreamEvent::ToolInputDelta { id, partial_json })
                if id == "call_1" && partial_json == "{\"path\":\"/tmp/a\"}"
        ));

        let done = converter.process(&event(serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 1,
            "item": {
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "read_file",
                "arguments": "{\"path\":\"/tmp/a\"}",
                "thought_signature": "opaque-thought-signature"
            }
        })));
        assert!(matches!(
            &done[0],
            Ok(StreamEvent::ToolState {
                tool_use_id,
                thought_signature,
            }) if tool_use_id == "call_1" && thought_signature == "opaque-thought-signature"
        ));
        assert!(matches!(
            &done[1],
            Ok(StreamEvent::ToolUse(ToolCall { id, name, input }))
                if id == "call_1" && name == "read_file" && input["path"] == "/tmp/a"
        ));

        let terminal = converter.process(&event(serde_json::json!({
            "type": "response.completed",
            "response": {
                "model": "gpt-5.6-sol",
                "status": "completed",
                "output": [{
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"/tmp/a\"}",
                    "thought_signature": "opaque-thought-signature"
                }],
                "usage": {
                    "input_tokens": 20,
                    "output_tokens": 5,
                    "input_tokens_details": {"cached_tokens": 3}
                }
            }
        })));
        assert_eq!(
            terminal
                .iter()
                .filter(|event| matches!(event, Ok(StreamEvent::ToolUse(_))))
                .count(),
            0,
            "terminal response must not re-emit an already completed tool call"
        );
        assert!(matches!(
            terminal.last(),
            Some(Ok(StreamEvent::Done { finish, usage }))
                if *finish == FinishReason::ToolUse
                    && usage.input_tokens == 20
                    && usage.output_tokens == 5
                    && usage.cache_read_tokens == 3
        ));
    }

    #[tokio::test]
    async fn responses_stream_rejects_eof_without_terminal_event() {
        use futures_util::StreamExt;

        let body = bytes::Bytes::from_static(
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
        );
        let bytes = futures_util::stream::iter(vec![Ok::<_, reqwest::Error>(body)]);
        let stream = responses_wire::ResponsesStream::new(bytes, "gpt-5.6-sol".into(), None, None);
        let events: Vec<_> = stream.collect().await;
        assert!(matches!(
            events.last(),
            Some(Err(LlmError::UpstreamMalformed(message)))
                if message.contains("before a terminal event")
        ));
    }

    #[test]
    fn responses_done_marker_reports_tool_use_finish() {
        use crate::agent::llm::sse::SseEvent;

        let event = |data: serde_json::Value| SseEvent {
            event: "message".into(),
            data: data.to_string(),
        };
        let mut converter = responses_wire::ResponsesStreamConverter::new("gpt-5.6-sol".into());
        converter.process(&event(serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "read_file",
                "arguments": "{}"
            }
        })));
        let done = converter.process(&SseEvent {
            event: "message".into(),
            data: "[DONE]".into(),
        });
        assert!(matches!(
            done.last(),
            Some(Ok(StreamEvent::Done {
                finish: FinishReason::ToolUse,
                ..
            }))
        ));
    }

    #[test]
    fn pool_classifies_copilot_preflight_auth_failures() {
        assert_eq!(
            pool_failure_class(&LlmError::Auth),
            crate::agent::llm::credential_pool::FailureClass::CooldownWorthy
        );
        assert_eq!(
            pool_failure_class(&LlmError::UpstreamMalformed("catalog".into())),
            crate::agent::llm::credential_pool::FailureClass::Transient
        );
    }

    /// HIGH-4: the streaming converter must surface each delta as
    /// soon as it parses, not buffer them all until [DONE]. This
    /// exercises `OpenAiStreamConverter::process` directly: feed
    /// three deltas + [DONE] and assert the output order is
    /// `TextDelta("Hel")`, `TextDelta("lo, ")`, `TextDelta("world!")`,
    /// `Done`.
    #[test]
    fn streaming_emits_incrementally() {
        use crate::agent::llm::sse::SseEvent;
        let mut conv = wire::OpenAiStreamConverter::new("gpt-4o-mini".into());

        let mk = |body: &str| SseEvent {
            event: "message".into(),
            data: body.into(),
        };

        let mut out: Vec<StreamEvent> = Vec::new();
        for chunk in [
            r#"{"choices":[{"delta":{"content":"Hel"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"content":"lo, "},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"content":"world!"},"finish_reason":"stop"}]}"#,
        ] {
            for e in conv.process(&mk(chunk)) {
                out.push(e.expect("delta should parse"));
            }
        }
        // The text deltas must have surfaced BEFORE we see [DONE].
        let texts: Vec<&str> = out
            .iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec!["Hel", "lo, ", "world!"],
            "deltas should stream in order"
        );
        // No Done yet — it only arrives on [DONE] / finish_stream.
        assert!(
            !out.iter().any(|e| matches!(e, StreamEvent::Done { .. })),
            "Done event must wait for [DONE]"
        );
        // Now feed [DONE].
        for e in conv.process(&mk("[DONE]")) {
            out.push(e.expect("done should parse"));
        }
        let last = out.last().expect("at least one event");
        match last {
            StreamEvent::Done { finish, .. } => assert!(matches!(finish, FinishReason::Stop)),
            other => panic!("expected Done, got {other:?}"),
        }
        // And the converter is now poisoned: further events are noops.
        assert!(conv.is_finished());
        assert!(conv.process(&mk("{}")).is_empty());
    }

    /// Malformed JSON in a streaming chunk must surface as
    /// `LlmError::UpstreamMalformed`, NOT as a silently dropped
    /// delta. The converter must also poison itself so subsequent
    /// chunks don't keep emitting.
    #[test]
    fn streaming_malformed_chunk_errors() {
        use crate::agent::llm::sse::SseEvent;
        let mut conv = wire::OpenAiStreamConverter::new("gpt-4o-mini".into());
        let sse = SseEvent {
            event: "message".into(),
            data: "this is not json".into(),
        };
        let out = conv.process(&sse);
        assert_eq!(out.len(), 1);
        assert!(
            matches!(out[0], Err(LlmError::UpstreamMalformed(_))),
            "got {:?}",
            out[0]
        );
        assert!(conv.is_finished());
    }
}
