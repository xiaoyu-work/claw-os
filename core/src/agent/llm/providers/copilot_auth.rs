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
use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex as AsyncMutex;

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

/// Editor identification headers required by the Copilot API. The
/// upstream rejects requests without them (and uses them for telemetry
/// + entitlement gating).
pub const EDITOR_VERSION: &str = "vscode/1.96.2";
pub const COPILOT_INTEGRATION_ID: &str = "vscode-chat";

/// Refresh the Copilot token this far ahead of its real expiry. Keeps a
/// long chat from failing mid-stream when the cached token ages out.
const REFRESH_SAFETY_MARGIN: Duration = Duration::from_secs(5 * 60);

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum CopilotAuthError {
    Network(String),
    Http { status: u16, body: String },
    UnexpectedBody(String),
    NotAuthorized(String),
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
/// `cos agent setup llm oauth-start` subcommand can serialize it
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
    let body = [("client_id", COPILOT_CLIENT_ID), ("scope", SCOPES)];
    let resp = http_client()
        .post(DEVICE_CODE_URL)
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
    let body = [
        ("client_id", COPILOT_CLIENT_ID),
        ("device_code", device_code),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
    ];
    let resp = http_client()
        .post(ACCESS_TOKEN_URL)
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
    let resp = http_client()
        .get(COPILOT_TOKEN_URL)
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
    let base_url = derive_base_url_from_token(&parsed.token);
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
pub async fn ensure_copilot_token(
    github_token: &str,
) -> Result<CopilotToken, CopilotAuthError> {
    let fingerprint = token_fingerprint(github_token);

    // Fast path: cache already has a usable token.
    if let Some(cached) = lookup_cached(fingerprint) {
        if !needs_refresh(&cached) {
            return Ok(cached);
        }
    }

    // Slow path: serialise the exchange per token. Only the first
    // caller hits the network; the rest awaits this lock and then
    // re-checks the cache.
    let lock = exchange_lock_for(fingerprint);
    let _guard = lock.lock().await;

    if let Some(cached) = lookup_cached(fingerprint) {
        if !needs_refresh(&cached) {
            return Ok(cached);
        }
    }
    let fresh = exchange_for_copilot_token(github_token).await?;
    store_cached(fingerprint, fresh.clone());
    Ok(fresh)
}

/// Drop any cached Copilot token for the given GitHub token. Called by
/// the sign-out path so a re-signed user gets a clean cache.
pub fn forget_cached(github_token: &str) {
    let fp = token_fingerprint(github_token);
    if let Some(map) = cache().lock().ok().as_mut() {
        map.remove(&fp);
    }
}

fn needs_refresh(t: &CopilotToken) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let margin = REFRESH_SAFETY_MARGIN.as_secs();
    t.expires_at_unix <= now.saturating_add(margin)
}

fn lookup_cached(fingerprint: u64) -> Option<CopilotToken> {
    cache().lock().ok().and_then(|m| m.get(&fingerprint).cloned())
}

fn store_cached(fingerprint: u64, token: CopilotToken) {
    if let Ok(mut m) = cache().lock() {
        m.insert(fingerprint, token);
    }
}

fn cache() -> &'static Mutex<HashMap<u64, CopilotToken>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, CopilotToken>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Per-fingerprint async mutex used to serialise concurrent token
/// exchanges against the same GitHub token. We keep one mutex per
/// fingerprint forever — they're tiny and bounded by the number of
/// distinct users signed in within this process.
fn exchange_lock_for(fingerprint: u64) -> Arc<AsyncMutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<u64, Arc<AsyncMutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut g = locks.lock().unwrap_or_else(|e| e.into_inner());
    g.entry(fingerprint)
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone()
}

/// Stable in-process fingerprint of a GitHub token. We deliberately
/// pin the seed so two lookups of the same token in this process map
/// to the same bucket — but each process gets its own seed (we
/// `random_state()` once via `HashMap::default()`), so the value can't
/// be replayed across processes. Used only as a cache key.
fn token_fingerprint(github_token: &str) -> u64 {
    static HASHER: OnceLock<std::collections::hash_map::RandomState> = OnceLock::new();
    let state = HASHER.get_or_init(std::collections::hash_map::RandomState::new);
    let mut h = state.build_hasher();
    github_token.hash(&mut h);
    h.finish()
}

/// Pull the Copilot API base URL out of the token string. The token's
/// metadata fragment looks like `tid=…;exp=…;proxy-ep=proxy.individual.githubcopilot.com;…`.
/// We rewrite `proxy.` → `api.` to get the chat-completions host.
/// Falls back to [`DEFAULT_COPILOT_BASE_URL`] if no `proxy-ep` is found.
pub fn derive_base_url_from_token(copilot_token: &str) -> String {
    for fragment in copilot_token.split(';') {
        let frag = fragment.trim();
        if let Some(rest) = frag.strip_prefix("proxy-ep=") {
            let host = rest.trim().trim_start_matches("https://").trim_start_matches("http://");
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
    DEFAULT_COPILOT_BASE_URL.to_string()
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("cos-agent/", env!("CARGO_PKG_VERSION")))
            // Per-phase timeouts: tighten the connect window so the
            // OAuth / token-exchange path can't stall the agent on a
            // dead-network race; cap the overall request at 30s.
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_base_url_picks_proxy_ep() {
        let tok =
            "tid=abc;exp=1700000000;proxy-ep=proxy.business.githubcopilot.com;sku=enterprise";
        assert_eq!(
            derive_base_url_from_token(tok),
            "https://api.business.githubcopilot.com"
        );
    }

    #[test]
    fn derive_base_url_strips_scheme_prefix() {
        let tok = "proxy-ep=https://proxy.individual.githubcopilot.com";
        assert_eq!(
            derive_base_url_from_token(tok),
            "https://api.individual.githubcopilot.com"
        );
    }

    #[test]
    fn derive_base_url_passthrough_non_proxy_host() {
        // If the token doesn't follow the `proxy.<region>` convention we
        // honour whatever it provides rather than guessing.
        let tok = "proxy-ep=custom.copilot.example.com";
        assert_eq!(
            derive_base_url_from_token(tok),
            "https://custom.copilot.example.com"
        );
    }

    #[test]
    fn derive_base_url_falls_back_when_proxy_ep_missing() {
        assert_eq!(
            derive_base_url_from_token("tid=abc;exp=1700000000"),
            DEFAULT_COPILOT_BASE_URL
        );
        assert_eq!(
            derive_base_url_from_token(""),
            DEFAULT_COPILOT_BASE_URL
        );
    }

    #[test]
    fn fingerprint_is_stable_and_sensitive() {
        let a = token_fingerprint("ghu_aaaaaa");
        let b = token_fingerprint("ghu_aaaaaa");
        let c = token_fingerprint("ghu_bbbbbb");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn cache_is_isolated_per_token() {
        let fp_a = token_fingerprint("cachetest_token_aaaaa");
        let fp_b = token_fingerprint("cachetest_token_bbbbb");
        store_cached(
            fp_a,
            CopilotToken {
                bearer: "tok_a".into(),
                base_url: "https://api.individual.githubcopilot.com".into(),
                expires_at_unix: u64::MAX,
            },
        );
        assert!(lookup_cached(fp_a).is_some());
        assert!(lookup_cached(fp_b).is_none());
    }

    #[test]
    fn needs_refresh_when_close_to_expiry() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let stale = CopilotToken {
            bearer: "x".into(),
            base_url: "https://api.individual.githubcopilot.com".into(),
            expires_at_unix: now + 60, // less than the 5-min margin
        };
        let fresh = CopilotToken {
            bearer: "x".into(),
            base_url: "https://api.individual.githubcopilot.com".into(),
            expires_at_unix: now + 60 * 60, // 1h ahead
        };
        assert!(needs_refresh(&stale));
        assert!(!needs_refresh(&fresh));
    }

    /// Regression: `truncate` used to do `&s[..n]`, which panics
    /// when `n` lands inside a multi-byte UTF-8 sequence. A 240-byte
    /// truncation of an error body that happens to contain CJK
    /// characters around byte 240 would crash the LLM request path
    /// instead of surfacing the upstream error.
    #[test]
    fn truncate_handles_non_ascii() {
        // Each '配' is 3 bytes in UTF-8. With max=4 the old impl would
        // try to slice at byte index 4 (mid-character) and panic.
        let s = "配额不足配额不足"; // 24 bytes, 8 chars
        let out = truncate(s, 4);
        // Exactly 4 chars + ellipsis.
        assert_eq!(out.chars().count(), 5);
        assert!(out.ends_with('…'));
        // And the prefix is the first four characters intact.
        assert!(out.starts_with("配额不足"));

        // Boundary cases that previously panicked.
        for n in [1usize, 2, 3, 5, 7] {
            // Must not panic.
            let _ = truncate(s, n);
        }

        // ASCII fast path still works.
        assert_eq!(truncate("hello", 100), "hello");
        assert!(truncate("hello world", 5).starts_with("hello"));
    }

    /// Sanity-check the exchange-mutex helper: two acquisitions for
    /// the same fingerprint return the same underlying Arc, while
    /// different fingerprints get different locks.
    #[tokio::test]
    async fn exchange_lock_is_per_fingerprint() {
        let a1 = exchange_lock_for(1);
        let a2 = exchange_lock_for(1);
        let b = exchange_lock_for(2);
        assert!(Arc::ptr_eq(&a1, &a2), "same fingerprint must share lock");
        assert!(!Arc::ptr_eq(&a1, &b), "different fingerprints must NOT share lock");

        // Holding the lock for fingerprint 1 must not block fingerprint 2.
        let g = a1.lock().await;
        let started = std::time::Instant::now();
        let _g2 = b.try_lock().expect("different lock must be free");
        assert!(started.elapsed() < std::time::Duration::from_millis(50));
        drop(g);
    }
}
