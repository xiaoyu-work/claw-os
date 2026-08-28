//! GitHub Copilot OAuth + token-exchange helpers.
//!
//! Two-tier auth model (mirrors the official Copilot clients):
//!
//!   1. Long-lived **GitHub OAuth token** acquired via the GitHub
//!      device-authorization flow. Persisted by [`crate::agent::setup`]
//!      under the credential name `copilot_github_token` in the
//!      `agent` namespace.
//!
//!   2. Short-lived **Copilot API token** (~30 min TTL) acquired by
//!      `GET https://api.github.com/copilot_internal/v2/token` with the
//!      GitHub token as bearer. Never persisted — re-exchanged on demand
//!      and cached in-process here.
//!
//! The Copilot token's value embeds a `proxy-ep=<host>` parameter that
//! tells us which proxy hostname to dial. We strip the `proxy.` prefix
//! and replace it with `api.` to land on the OpenAI-compatible
//! chat-completions endpoint
//! (`https://api.<region>.githubcopilot.com/chat/completions`).
//!
//! Anything inside this module that touches the wire path returns
//! [`CopilotAuthError`] so callers (the openai_compat alias, the
//! `cos agent setup` subcommands) can render structured error envelopes.
//!
//! No public types from this module are persisted. The cache is wiped
//! on process restart.

use serde::Deserialize;
use std::collections::HashMap;
use std::future::Future;
use std::hash::BuildHasher;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex as AsyncMutex;

use crate::agent::llm::construction::HttpTransport;

/// GitHub OAuth client ID for Copilot's first-party application.
///
/// Same value the official VS Code Copilot extension uses. Public by design
/// — device-flow client IDs are not secrets, the auth boundary is the user
/// approving the device code on github.com.
pub const COPILOT_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const DEFAULT_COPILOT_BASE_URL: &str = "https://api.individual.githubcopilot.com";
const SCOPES: &str = "read:user";
const AUTH_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub(crate) struct CopilotAuthEndpoints {
    token_url: String,
    fallback_api_base_url: String,
}

impl Default for CopilotAuthEndpoints {
    fn default() -> Self {
        Self {
            token_url: COPILOT_TOKEN_URL.to_string(),
            fallback_api_base_url: DEFAULT_COPILOT_BASE_URL.to_string(),
        }
    }
}

#[cfg(test)]
impl CopilotAuthEndpoints {
    pub(crate) fn for_test(
        token_url: impl Into<String>,
        fallback_api_base_url: impl Into<String>,
    ) -> Self {
        Self {
            token_url: token_url.into(),
            fallback_api_base_url: fallback_api_base_url.into(),
        }
    }
}

/// Editor identification headers required by the Copilot API. The
/// upstream rejects requests without them (and uses them for telemetry
/// + entitlement gating).
pub const EDITOR_VERSION: &str = "vscode/1.96.2";
pub const COPILOT_INTEGRATION_ID: &str = "vscode-chat";
pub const GITHUB_API_VERSION: &str = "2025-10-01";
pub const COPILOT_INITIATOR_USER: &str = "user";
pub const COPILOT_INITIATOR_AGENT: &str = "agent";
pub const COPILOT_INTERACTION_TYPE: &str = "conversation-agent";

/// Refresh the Copilot token this far ahead of its real expiry. Keeps a
/// long chat from failing mid-stream when the cached token ages out.
const REFRESH_SAFETY_MARGIN: Duration = Duration::from_secs(5 * 60);
const MODEL_CATALOG_TTL: Duration = Duration::from_secs(5 * 60);

/// Copilot wire protocols that `cos` can speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopilotWireApi {
    ChatCompletions,
    Responses,
}

impl CopilotWireApi {
    pub fn endpoint_path(self) -> &'static str {
        match self {
            Self::ChatCompletions => "/chat/completions",
            Self::Responses => "/responses",
        }
    }

    pub fn config_name(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
        }
    }
}

/// One entry from Copilot's live `/models` catalogue.
///
/// Older entries omit `supported_endpoints`; those are ordinary
/// chat-completions models. A non-empty endpoint list is authoritative.
#[derive(Debug, Clone, Deserialize)]
pub struct CopilotModel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    model_picker_enabled: Option<bool>,
    #[serde(default)]
    supported_endpoints: Option<Vec<String>>,
    #[serde(default)]
    capabilities: Option<CopilotModelCapabilities>,
    #[serde(default)]
    policy: Option<CopilotModelPolicy>,
}

#[derive(Debug, Clone, Deserialize)]
struct CopilotModelCapabilities {
    #[serde(rename = "type")]
    #[serde(default)]
    model_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CopilotModelPolicy {
    #[serde(default)]
    state: Option<String>,
}

impl CopilotModel {
    /// Select the newest protocol the model explicitly advertises.
    ///
    /// Older catalogue entries omit endpoint metadata; those retain the
    /// legacy chat-completions fallback. When both are available, Responses
    /// wins so models get the richer reasoning and tool-call transport.
    pub fn wire_api(&self) -> Option<CopilotWireApi> {
        let endpoints = self.supported_endpoints.as_deref().unwrap_or_default();
        if endpoints.is_empty() {
            return Some(CopilotWireApi::ChatCompletions);
        }
        if endpoints.iter().any(|v| v == "/responses") {
            return Some(CopilotWireApi::Responses);
        }
        if endpoints.iter().any(|v| v == "/chat/completions") {
            return Some(CopilotWireApi::ChatCompletions);
        }
        None
    }

    pub fn is_selectable_chat_model(&self) -> bool {
        if self.model_picker_enabled != Some(true) {
            return false;
        }
        if self
            .policy
            .as_ref()
            .and_then(|p| p.state.as_deref())
            .is_some_and(|state| !state.eq_ignore_ascii_case("enabled"))
        {
            return false;
        }
        if !self
            .capabilities
            .as_ref()
            .and_then(|c| c.model_type.as_deref())
            .is_some_and(|kind| kind.eq_ignore_ascii_case("chat"))
        {
            return false;
        }
        self.wire_api().is_some()
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum CopilotAuthError {
    Network(String),
    Http { status: u16, body: String },
    UnexpectedBody(String),
    NotAuthorized(String),
    UnsupportedModel(String),
    StateUnavailable { resource: &'static str },
}

impl std::fmt::Display for CopilotAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CopilotAuthError::Network(e) => write!(f, "network error: {e}"),
            CopilotAuthError::Http { status, body } => {
                write!(f, "HTTP {status}: {}", truncate(body, 240))
            }
            CopilotAuthError::UnexpectedBody(s) => write!(f, "unexpected response body: {s}"),
            CopilotAuthError::NotAuthorized(s) => write!(f, "{s}"),
            CopilotAuthError::UnsupportedModel(s) => write!(f, "{s}"),
            CopilotAuthError::StateUnavailable { resource } => {
                write!(f, "Copilot {resource} state is unavailable")
            }
        }
    }
}

impl std::error::Error for CopilotAuthError {}

impl From<reqwest::Error> for CopilotAuthError {
    fn from(e: reqwest::Error) -> Self {
        CopilotAuthError::Network(e.to_string())
    }
}

fn truncate(s: &str, max: usize) -> String {
    // Use char-boundary truncation. The previous implementation did
    // `&s[..max]` which panics when `max` lands inside a multi-byte
    // UTF-8 sequence — a real bug for non-ASCII error bodies (e.g. a
    // 240-byte cap into a Chinese / Japanese error message would
    // crash the LLM request path).
    crate::agent::llm::truncate_for_display(s, max)
}

// ---------------------------------------------------------------------------
// Device flow
// ---------------------------------------------------------------------------

/// Server response to a device-code request. Public so the
/// `cos agent setup text oauth-start` subcommand can serialize it
/// straight to JSON.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    /// Seconds the client must wait between polls. GitHub sometimes
    /// returns 5; we honour `slow_down` responses by bumping locally.
    pub interval: u64,
}

/// One-shot poll result. The CLI loops on this externally so each
/// invocation returns immediately — keeping the kernel responsive and
/// the polling cadence controlled by the UI.
#[derive(Debug, Clone)]
pub enum PollOutcome {
    /// Still waiting for the user to approve on github.com.
    Pending,
    /// Server asked us to slow down. New polling interval, in seconds.
    SlowDown { interval: u64 },
    /// User authorized. Long-lived GitHub OAuth token is included —
    /// caller is responsible for persisting it via the credential store.
    Authorized {
        github_token: String,
        token_type: String,
        scope: String,
    },
    /// Device code expired before the user approved.
    Expired,
    /// User denied the authorization request on github.com.
    Denied,
}

/// Request a fresh device code. Caller displays `user_code` to the
/// user along with `verification_uri` and polls
/// [`poll_device_flow`] every `interval` seconds.
pub async fn start_device_flow() -> Result<DeviceCode, CopilotAuthError> {
    start_device_flow_with_transport(legacy_http_transport()).await
}

pub async fn start_device_flow_with_transport(
    transport: &HttpTransport,
) -> Result<DeviceCode, CopilotAuthError> {
    let body = [("client_id", COPILOT_CLIENT_ID), ("scope", SCOPES)];
    let resp = transport
        .post(DEVICE_CODE_URL, AUTH_HTTP_TIMEOUT)
        .header("Accept", "application/json")
        .form(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(CopilotAuthError::Http {
            status: status.as_u16(),
            body: text,
        });
    }
    serde_json::from_str::<DeviceCode>(&text)
        .map_err(|e| CopilotAuthError::UnexpectedBody(format!("{e}: {}", truncate(&text, 240))))
}

#[derive(Debug, Deserialize)]
struct TokenPollResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Single poll against the GitHub access-token endpoint. Returns
/// immediately — the caller loops with its own scheduler.
pub async fn poll_device_flow(device_code: &str) -> Result<PollOutcome, CopilotAuthError> {
    poll_device_flow_with_transport(device_code, legacy_http_transport()).await
}

pub async fn poll_device_flow_with_transport(
    device_code: &str,
    transport: &HttpTransport,
) -> Result<PollOutcome, CopilotAuthError> {
    let body = [
        ("client_id", COPILOT_CLIENT_ID),
        ("device_code", device_code),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
    ];
    let resp = transport
        .post(ACCESS_TOKEN_URL, AUTH_HTTP_TIMEOUT)
        .header("Accept", "application/json")
        .form(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    // GitHub returns HTTP 200 with an `error` field for the pending /
    // slow_down / expired / denied cases. Only a non-2xx (or a totally
    // malformed body) is a hard error.
    if !status.is_success() {
        return Err(CopilotAuthError::Http {
            status: status.as_u16(),
            body: text,
        });
    }
    let parsed: TokenPollResponse = serde_json::from_str(&text)
        .map_err(|e| CopilotAuthError::UnexpectedBody(format!("{e}: {}", truncate(&text, 240))))?;
    if let Some(token) = parsed.access_token {
        return Ok(PollOutcome::Authorized {
            github_token: token,
            token_type: parsed.token_type.unwrap_or_else(|| "bearer".into()),
            scope: parsed.scope.unwrap_or_default(),
        });
    }
    match parsed.error.as_deref() {
        Some("authorization_pending") | None => Ok(PollOutcome::Pending),
        Some("slow_down") => Ok(PollOutcome::SlowDown { interval: 10 }),
        Some("expired_token") => Ok(PollOutcome::Expired),
        Some("access_denied") => Ok(PollOutcome::Denied),
        Some(other) => Err(CopilotAuthError::NotAuthorized(other.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Copilot token exchange + cache
// ---------------------------------------------------------------------------

/// A fresh Copilot API token plus the base URL derived from its
/// `proxy-ep=` parameter.
#[derive(Debug, Clone)]
pub struct CopilotToken {
    pub bearer: String,
    pub base_url: String,
    pub expires_at_unix: u64,
}

#[derive(Debug, Deserialize)]
struct CopilotTokenResponse {
    token: String,
    #[serde(default)]
    expires_at: u64,
}

/// Exchange a GitHub OAuth token for a short-lived Copilot API token.
/// Does not consult or update the cache — callers usually want
/// [`ensure_copilot_token`] instead.
pub async fn exchange_for_copilot_token(
    github_token: &str,
) -> Result<CopilotToken, CopilotAuthError> {
    exchange_for_copilot_token_with_transport(
        github_token,
        legacy_http_transport(),
        &CopilotAuthEndpoints::default(),
    )
    .await
}

pub(crate) async fn exchange_for_copilot_token_with_transport(
    github_token: &str,
    transport: &HttpTransport,
    endpoints: &CopilotAuthEndpoints,
) -> Result<CopilotToken, CopilotAuthError> {
    let resp = transport
        .get(&endpoints.token_url, AUTH_HTTP_TIMEOUT)
        .header("Accept", "application/json")
        .header("Editor-Version", EDITOR_VERSION)
        .header("Copilot-Integration-Id", COPILOT_INTEGRATION_ID)
        .bearer_auth(github_token)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(CopilotAuthError::Http {
            status: status.as_u16(),
            body: text,
        });
    }
    let parsed: CopilotTokenResponse = serde_json::from_str(&text)
        .map_err(|e| CopilotAuthError::UnexpectedBody(format!("{e}: {}", truncate(&text, 240))))?;
    let base_url =
        derive_base_url_from_token_with_fallback(&parsed.token, &endpoints.fallback_api_base_url);
    Ok(CopilotToken {
        bearer: parsed.token,
        base_url,
        expires_at_unix: parsed.expires_at,
    })
}

/// Resolve a usable Copilot token for the supplied GitHub token. Uses
/// the in-process cache when possible; re-exchanges with a 5-minute
/// safety margin when the cached value is close to expiry.
///
/// Concurrent callers for the *same* GitHub token coalesce on a
/// per-token `tokio::sync::Mutex`: the first caller does the network
/// exchange, the rest wait and share the freshly cached result. This
/// prevents the thundering-herd that used to happen when N parallel
/// chat requests started up against an empty / expired cache and all
/// raced into `exchange_for_copilot_token`, blowing the upstream
/// rate-limit and risking N tokens issued for the same user.
pub async fn ensure_copilot_token(github_token: &str) -> Result<CopilotToken, CopilotAuthError> {
    ensure_copilot_token_with_transport(
        github_token,
        legacy_http_transport(),
        &CopilotAuthEndpoints::default(),
    )
    .await
}

pub(crate) async fn ensure_copilot_token_with_transport(
    github_token: &str,
    transport: &HttpTransport,
    endpoints: &CopilotAuthEndpoints,
) -> Result<CopilotToken, CopilotAuthError> {
    let fingerprint = token_fingerprint(github_token);

    // Fast path: cache already has a usable token.
    if let Some(cached) = lookup_cached(fingerprint)? {
        if !needs_refresh(&cached) {
            return Ok(cached);
        }
    }

    // Slow path: serialise the exchange per token. Only the first
    // caller hits the network; the rest awaits this lock and then
    // re-checks the cache.
    let lock = exchange_lock_for(fingerprint)?;
    let _guard = lock.lock().await;

    if let Some(cached) = lookup_cached(fingerprint)? {
        if !needs_refresh(&cached) {
            return Ok(cached);
        }
    }
    let fresh =
        exchange_for_copilot_token_with_transport(github_token, transport, endpoints).await?;
    store_cached(fingerprint, fresh.clone())?;
    Ok(fresh)
}

/// Replace a short-lived Copilot token rejected by the API.
///
/// The long-lived GitHub credential is never removed here. Refreshes are
/// serialized with ordinary cache fills, and a concurrent caller that already
/// replaced `rejected_token` wins so only one exchange is needed.
pub async fn refresh_rejected_copilot_token(
    github_token: &str,
    rejected_token: &CopilotToken,
) -> Result<CopilotToken, CopilotAuthError> {
    refresh_rejected_copilot_token_with_transport(
        github_token,
        rejected_token,
        legacy_http_transport(),
        &CopilotAuthEndpoints::default(),
    )
    .await
}

pub(crate) async fn refresh_rejected_copilot_token_with_transport(
    github_token: &str,
    rejected_token: &CopilotToken,
    transport: &HttpTransport,
    endpoints: &CopilotAuthEndpoints,
) -> Result<CopilotToken, CopilotAuthError> {
    refresh_rejected_copilot_token_with(github_token, rejected_token, |token| async move {
        exchange_for_copilot_token_with_transport(&token, transport, endpoints).await
    })
    .await
}

async fn refresh_rejected_copilot_token_with<F, Fut>(
    github_token: &str,
    rejected_token: &CopilotToken,
    exchange: F,
) -> Result<CopilotToken, CopilotAuthError>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<CopilotToken, CopilotAuthError>>,
{
    let github_fingerprint = token_fingerprint(github_token);
    let exchange_lock = exchange_lock_for(github_fingerprint)?;
    let _exchange_guard = exchange_lock.lock().await;

    let rejected_fingerprint = token_fingerprint(&rejected_token.bearer);
    let catalog_lock = model_catalog_lock_for(rejected_fingerprint)?;
    let _catalog_guard = catalog_lock.lock().await;
    model_catalog_cache()
        .lock()
        .map_err(|_| CopilotAuthError::StateUnavailable {
            resource: "model catalog cache",
        })?
        .remove(&rejected_fingerprint);
    drop(_catalog_guard);

    {
        let mut tokens = cache()
            .lock()
            .map_err(|_| CopilotAuthError::StateUnavailable {
                resource: "token cache",
            })?;
        if let Some(current) = tokens.get(&github_fingerprint) {
            if current.bearer != rejected_token.bearer {
                return Ok(current.clone());
            }
        }
        tokens.remove(&github_fingerprint);
    }

    let fresh = exchange(github_token.to_string()).await?;
    store_cached(github_fingerprint, fresh.clone())?;
    Ok(fresh)
}

/// Drop any cached Copilot token for the given GitHub token. Called by
/// the sign-out path so a re-signed user gets a clean cache.
pub fn forget_cached(github_token: &str) {
    if let Err(error) = try_forget_cached(github_token) {
        tracing::error!(error = %error, "failed to clear Copilot token cache");
    }
}

pub fn try_forget_cached(github_token: &str) -> Result<(), CopilotAuthError> {
    let fp = token_fingerprint(github_token);
    cache()
        .lock()
        .map_err(|_| CopilotAuthError::StateUnavailable {
            resource: "token cache",
        })?
        .remove(&fp);
    Ok(())
}

/// Return the live model catalogue, cached for a short period per
/// short-lived Copilot bearer. Entitlements and endpoint availability
/// can change while the process is running, so this cache is deliberately
/// much shorter than the bearer lifetime.
pub async fn ensure_copilot_models(
    token: &CopilotToken,
) -> Result<Arc<Vec<CopilotModel>>, CopilotAuthError> {
    ensure_copilot_models_with_transport(token, legacy_http_transport()).await
}

pub(crate) async fn ensure_copilot_models_with_transport(
    token: &CopilotToken,
    transport: &HttpTransport,
) -> Result<Arc<Vec<CopilotModel>>, CopilotAuthError> {
    let fingerprint = token_fingerprint(&token.bearer);
    if let Some(models) = lookup_model_catalog(fingerprint)? {
        return Ok(models);
    }

    let lock = model_catalog_lock_for(fingerprint)?;
    let _guard = lock.lock().await;
    if let Some(models) = lookup_model_catalog(fingerprint)? {
        return Ok(models);
    }

    let models = Arc::new(fetch_copilot_models(token, transport).await?);
    store_model_catalog(fingerprint, models.clone())?;
    Ok(models)
}

/// Resolve the protocol for one configured model.
///
/// A manually-entered model that is absent from the live catalogue keeps
/// the historical chat-completions behaviour. An advertised model with an
/// unsupported endpoint is rejected before a doomed provider request.
pub async fn wire_api_for_model(
    token: &CopilotToken,
    model_id: &str,
) -> Result<CopilotWireApi, CopilotAuthError> {
    wire_api_for_model_with_transport(token, model_id, legacy_http_transport()).await
}

pub(crate) async fn wire_api_for_model_with_transport(
    token: &CopilotToken,
    model_id: &str,
    transport: &HttpTransport,
) -> Result<CopilotWireApi, CopilotAuthError> {
    let models = ensure_copilot_models_with_transport(token, transport).await?;
    let Some(model) = models.iter().find(|m| m.id == model_id) else {
        tracing::warn!(
            target: "cos::agent::llm::copilot",
            "Copilot model '{model_id}' was not present in the live catalogue; \
             falling back to chat completions"
        );
        return Ok(CopilotWireApi::ChatCompletions);
    };
    model.wire_api().ok_or_else(|| {
        CopilotAuthError::UnsupportedModel(format!(
            "Copilot model `{model_id}` is not usable by this client; advertised endpoints: {}",
            if model
                .supported_endpoints
                .as_ref()
                .is_none_or(|endpoints| endpoints.is_empty())
            {
                "<none>".to_string()
            } else {
                model
                    .supported_endpoints
                    .as_deref()
                    .unwrap_or_default()
                    .join(", ")
            }
        ))
    })
}

async fn fetch_copilot_models(
    token: &CopilotToken,
    transport: &HttpTransport,
) -> Result<Vec<CopilotModel>, CopilotAuthError> {
    let url = format!("{}/models", token.base_url.trim_end_matches('/'));
    let resp = transport
        .get(&url, AUTH_HTTP_TIMEOUT)
        .header("Accept", "application/json")
        .header("Editor-Version", EDITOR_VERSION)
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .header("Copilot-Integration-Id", COPILOT_INTEGRATION_ID)
        .bearer_auth(&token.bearer)
        .send()
        .await?;
    let status = resp.status();
    let bytes =
        crate::agent::llm::read_body_capped(resp, crate::agent::llm::MAX_NONSTREAM_BODY_BYTES)
            .await
            .map_err(|e| CopilotAuthError::UnexpectedBody(e.to_string()))?;
    if !status.is_success() {
        return Err(CopilotAuthError::Http {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }

    let parsed: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
        CopilotAuthError::UnexpectedBody(format!(
            "parse /models response: {e}: {}",
            truncate(&String::from_utf8_lossy(&bytes), 240)
        ))
    })?;
    let entries = parsed
        .get("data")
        .or_else(|| parsed.get("models"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            CopilotAuthError::UnexpectedBody(format!(
                "/models had no data/models array: {}",
                truncate(&String::from_utf8_lossy(&bytes), 240)
            ))
        })?;

    let mut models = Vec::with_capacity(entries.len());
    for entry in entries {
        match serde_json::from_value::<CopilotModel>(entry.clone()) {
            Ok(model) if !model.id.trim().is_empty() => models.push(model),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    target: "cos::agent::llm::copilot",
                    "ignoring malformed Copilot model entry: {error}"
                );
            }
        }
    }
    if !entries.is_empty() && models.is_empty() {
        return Err(CopilotAuthError::UnexpectedBody(
            "none of the Copilot model entries could be parsed".into(),
        ));
    }
    Ok(models)
}

struct CachedModelCatalog {
    fetched_at: Instant,
    models: Arc<Vec<CopilotModel>>,
}

fn lookup_model_catalog(
    fingerprint: u64,
) -> Result<Option<Arc<Vec<CopilotModel>>>, CopilotAuthError> {
    let cache = model_catalog_cache()
        .lock()
        .map_err(|_| CopilotAuthError::StateUnavailable {
            resource: "model catalog cache",
        })?;
    Ok(cache.get(&fingerprint).and_then(|entry| {
        (entry.fetched_at.elapsed() < MODEL_CATALOG_TTL).then(|| entry.models.clone())
    }))
}

fn store_model_catalog(
    fingerprint: u64,
    models: Arc<Vec<CopilotModel>>,
) -> Result<(), CopilotAuthError> {
    let mut cache =
        model_catalog_cache()
            .lock()
            .map_err(|_| CopilotAuthError::StateUnavailable {
                resource: "model catalog cache",
            })?;
    cache.retain(|_, entry| entry.fetched_at.elapsed() < Duration::from_secs(60 * 60));
    cache.insert(
        fingerprint,
        CachedModelCatalog {
            fetched_at: Instant::now(),
            models,
        },
    );
    Ok(())
}

fn model_catalog_cache() -> &'static Mutex<HashMap<u64, CachedModelCatalog>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, CachedModelCatalog>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn model_catalog_lock_for(fingerprint: u64) -> Result<Arc<AsyncMutex<()>>, CopilotAuthError> {
    static LOCKS: OnceLock<Mutex<HashMap<u64, Arc<AsyncMutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = locks
        .lock()
        .map_err(|_| CopilotAuthError::StateUnavailable {
            resource: "model catalog lock registry",
        })?;
    Ok(guard
        .entry(fingerprint)
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone())
}

fn needs_refresh(t: &CopilotToken) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let margin = REFRESH_SAFETY_MARGIN.as_secs();
    t.expires_at_unix <= now.saturating_add(margin)
}

fn lookup_cached(fingerprint: u64) -> Result<Option<CopilotToken>, CopilotAuthError> {
    Ok(cache()
        .lock()
        .map_err(|_| CopilotAuthError::StateUnavailable {
            resource: "token cache",
        })?
        .get(&fingerprint)
        .cloned())
}

fn store_cached(fingerprint: u64, token: CopilotToken) -> Result<(), CopilotAuthError> {
    cache()
        .lock()
        .map_err(|_| CopilotAuthError::StateUnavailable {
            resource: "token cache",
        })?
        .insert(fingerprint, token);
    Ok(())
}

fn cache() -> &'static Mutex<HashMap<u64, CopilotToken>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, CopilotToken>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Per-fingerprint async mutex used to serialise concurrent token
/// exchanges against the same GitHub token. We keep one mutex per
/// fingerprint forever — they're tiny and bounded by the number of
/// distinct users signed in within this process.
fn exchange_lock_for(fingerprint: u64) -> Result<Arc<AsyncMutex<()>>, CopilotAuthError> {
    static LOCKS: OnceLock<Mutex<HashMap<u64, Arc<AsyncMutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut g = locks
        .lock()
        .map_err(|_| CopilotAuthError::StateUnavailable {
            resource: "token exchange lock registry",
        })?;
    Ok(g.entry(fingerprint)
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone())
}

/// Stable in-process fingerprint of a GitHub token. We deliberately
/// pin the seed so two lookups of the same token in this process map
/// to the same bucket — but each process gets its own seed (we
/// `random_state()` once via `HashMap::default()`), so the value can't
/// be replayed across processes. Used only as a cache key.
fn token_fingerprint(github_token: &str) -> u64 {
    static HASHER: OnceLock<std::collections::hash_map::RandomState> = OnceLock::new();
    let state = HASHER.get_or_init(std::collections::hash_map::RandomState::new);
    state.hash_one(github_token)
}

/// Pull the Copilot API base URL out of the token string. The token's
/// metadata fragment looks like `tid=…;exp=…;proxy-ep=proxy.individual.githubcopilot.com;…`.
/// We rewrite `proxy.` → `api.` to get the chat-completions host.
/// Falls back to [`DEFAULT_COPILOT_BASE_URL`] if no `proxy-ep` is found.
pub fn derive_base_url_from_token(copilot_token: &str) -> String {
    derive_base_url_from_token_with_fallback(copilot_token, DEFAULT_COPILOT_BASE_URL)
}

fn derive_base_url_from_token_with_fallback(
    copilot_token: &str,
    fallback_api_base_url: &str,
) -> String {
    for fragment in copilot_token.split(';') {
        let frag = fragment.trim();
        if let Some(rest) = frag.strip_prefix("proxy-ep=") {
            let host = rest
                .trim()
                .trim_start_matches("https://")
                .trim_start_matches("http://");
            if host.is_empty() {
                break;
            }
            let api_host = match host.strip_prefix("proxy.") {
                Some(rest) => format!("api.{rest}"),
                None => host.to_string(),
            };
            return format!("https://{api_host}");
        }
    }
    fallback_api_base_url.to_string()
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

fn legacy_http_transport() -> &'static HttpTransport {
    static TRANSPORT: OnceLock<HttpTransport> = OnceLock::new();
    TRANSPORT.get_or_init(HttpTransport::legacy_default)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/providers/copilot_auth.rs"
    ));
}
