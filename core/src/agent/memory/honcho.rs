//! Honcho dialectic user-model client.
//!
//! Honcho (<https://honcho.dev>) is the only retained external memory
//! plugin in ClawOS — it provides representational learning about
//! users from conversation history and exposes a "dialectic" query
//! API that an agent can use to enrich prompts with what's known
//! about the current user.
//!
//! ## Scope
//!
//! This module is a *minimal* HTTP client. It owns:
//!
//! - [`HonchoConfig`] — connection config (base URL, API key,
//!   workspace, optional peer/session prefixes).
//! - [`HonchoClient`] — async client over `reqwest`, with two
//!   operations:
//!   - [`HonchoClient::append_message`] — record a turn into a
//!     session.
//!   - [`HonchoClient::dialectic_query`] — ask Honcho's engine for
//!     facts/insights about a peer.
//! - [`HonchoError`] — typed error surface (config / network /
//!   protocol).
//!
//! ## Defaults & opt-in
//!
//! Honcho is **opt-in**. [`HonchoConfig::from_env`] returns `None`
//! unless `HONCHO_BASE_URL` is set. When unconfigured, the runtime
//! must skip Honcho calls entirely — the rest of the agent works
//! identically without it.
//!
//! ## Resilience
//!
//! Honcho being slow or down MUST NOT crash the agent. Callers wrap
//! every method in their own error policy (typically: log a warning
//! and proceed without enrichment). The default request timeout is
//! 10s; overridable via [`HonchoConfig::timeout_secs`].
//!
//! ## API surface assumed
//!
//! This implementation targets Honcho's v1 HTTP API:
//!
//! - POST `{base}/workspaces/{workspace_id}/sessions/{session_id}/messages`
//!   — body `{"messages": [{"peer_id": "...", "content": "..."}]}`
//! - POST `{base}/workspaces/{workspace_id}/peers/{peer_id}/chat`
//!   — body `{"queries": ["..."], "session_id": "...", "stream": false}`
//!   returns `{"content": "..."}`
//!
//! Both endpoints accept `Authorization: Bearer <api_key>` when
//! configured. The exact endpoint paths are constructed in
//! [`HonchoClient::messages_url`] / [`HonchoClient::chat_url`] so
//! tests can assert on them.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Honcho client configuration. Build from env via
/// [`HonchoConfig::from_env`] or assemble manually for tests.
#[derive(Debug, Clone)]
pub struct HonchoConfig {
    /// Base URL of the Honcho API (no trailing slash). Examples:
    ///   - `https://api.honcho.dev/v1`
    ///   - `http://localhost:8000/v1` (self-hosted)
    pub base_url: String,
    /// Optional bearer token. Self-hosted dev instances may not
    /// require auth.
    pub api_key: Option<String>,
    /// Workspace identifier; Honcho's top-level container.
    pub workspace_id: String,
    /// Per-request timeout in seconds. Default 10.
    pub timeout_secs: u64,
}

impl HonchoConfig {
    /// Build from environment. Returns `None` when `HONCHO_BASE_URL`
    /// is unset (i.e. Honcho is disabled).
    ///
    /// Recognised env vars:
    ///   - `HONCHO_BASE_URL` (required for any return value)
    ///   - `HONCHO_API_KEY` (optional)
    ///   - `HONCHO_WORKSPACE_ID` (defaults to `"default"`)
    ///   - `HONCHO_TIMEOUT_SECS` (defaults to `10`; non-numeric values
    ///     are ignored)
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var("HONCHO_BASE_URL").ok()?.trim().to_string();
        if base_url.is_empty() {
            return None;
        }
        let api_key = std::env::var("HONCHO_API_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let workspace_id = std::env::var("HONCHO_WORKSPACE_ID")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "default".to_string());
        let timeout_secs = std::env::var("HONCHO_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(10);
        Some(Self {
            base_url: trim_trailing_slash(&base_url),
            api_key,
            workspace_id,
            timeout_secs,
        })
    }
}

/// Strip exactly one trailing `/` so `{base}/workspaces/...` always
/// joins cleanly.
fn trim_trailing_slash(s: &str) -> String {
    s.strip_suffix('/').unwrap_or(s).to_string()
}

/// Role of a single message recorded into Honcho. Honcho itself uses
/// peer-centric terminology rather than role labels, but we expose a
/// boolean shorthand for the common "user vs assistant" case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
}

/// Errors returned by [`HonchoClient`] methods.
#[derive(Debug)]
pub enum HonchoError {
    /// Misuse: empty session id, peer id, etc.
    Config(String),
    /// reqwest-level transport / timeout failure.
    Network(String),
    /// HTTP non-2xx response. Includes status and body for diagnostics.
    Http { status: u16, body: String },
    /// Body did not match expected JSON shape.
    Protocol(String),
}

impl std::fmt::Display for HonchoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(s) => write!(f, "honcho config error: {s}"),
            Self::Network(s) => write!(f, "honcho network error: {s}"),
            Self::Http { status, body } => {
                let preview: String = body.chars().take(200).collect();
                write!(f, "honcho http {status}: {preview}")
            }
            Self::Protocol(s) => write!(f, "honcho protocol error: {s}"),
        }
    }
}

impl std::error::Error for HonchoError {}

/// Minimal async client over Honcho's v1 HTTP API.
#[derive(Debug, Clone)]
pub struct HonchoClient {
    config: HonchoConfig,
    http: reqwest::Client,
}

impl HonchoClient {
    /// Build a client. Returns an error only if the underlying
    /// `reqwest::Client` builder fails (rare; e.g. no TLS provider).
    pub fn new(config: HonchoConfig) -> Result<Self, HonchoError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| HonchoError::Network(format!("client build: {e}")))?;
        Ok(Self { config, http })
    }

    /// Read the configured timeout. Useful for tests / status output.
    pub fn timeout_secs(&self) -> u64 {
        self.config.timeout_secs
    }

    /// Read the configured base URL.
    pub fn base_url(&self) -> &str {
        &self.config.base_url
    }

    /// URL for posting a message into a session.
    pub fn messages_url(&self, session_id: &str) -> String {
        format!(
            "{}/workspaces/{}/sessions/{}/messages",
            self.config.base_url, self.config.workspace_id, session_id
        )
    }

    /// URL for the dialectic chat query against a peer.
    pub fn chat_url(&self, peer_id: &str) -> String {
        format!(
            "{}/workspaces/{}/peers/{}/chat",
            self.config.base_url, self.config.workspace_id, peer_id
        )
    }

    /// Append one message to a session.
    ///
    /// The `peer_id` identifies the speaker (typically the user's
    /// stable id for `User` messages, and the agent's id for
    /// `Assistant` messages — Honcho models both as peers).
    ///
    /// Returns `Ok(())` on a 2xx response; the response body is
    /// ignored.
    pub async fn append_message(
        &self,
        session_id: &str,
        peer_id: &str,
        content: &str,
        role: MessageRole,
    ) -> Result<(), HonchoError> {
        if session_id.is_empty() {
            return Err(HonchoError::Config("session_id is empty".into()));
        }
        if peer_id.is_empty() {
            return Err(HonchoError::Config("peer_id is empty".into()));
        }
        let url = self.messages_url(session_id);
        let body = MessagesRequest {
            messages: vec![MessageIn {
                peer_id: peer_id.to_string(),
                content: content.to_string(),
                role: match role {
                    MessageRole::User => "user".to_string(),
                    MessageRole::Assistant => "assistant".to_string(),
                },
            }],
        };
        let mut req = self.http.post(&url).json(&body);
        if let Some(key) = &self.config.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| HonchoError::Network(format!("POST {url}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(HonchoError::Http {
                status: status.as_u16(),
                body,
            });
        }
        Ok(())
    }

    /// Run a dialectic chat query about a peer, returning the
    /// content string Honcho replies with.
    ///
    /// `session_id` is optional; when supplied, Honcho scopes the
    /// reasoning to that session's history. When `None`, Honcho uses
    /// its global model of the peer.
    pub async fn dialectic_query(
        &self,
        peer_id: &str,
        query: &str,
        session_id: Option<&str>,
    ) -> Result<String, HonchoError> {
        if peer_id.is_empty() {
            return Err(HonchoError::Config("peer_id is empty".into()));
        }
        if query.trim().is_empty() {
            return Err(HonchoError::Config("query is empty".into()));
        }
        let url = self.chat_url(peer_id);
        let body = ChatRequest {
            queries: vec![query.to_string()],
            session_id: session_id.map(|s| s.to_string()),
            stream: false,
        };
        let mut req = self.http.post(&url).json(&body);
        if let Some(key) = &self.config.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| HonchoError::Network(format!("POST {url}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(HonchoError::Http {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: ChatResponse = resp
            .json()
            .await
            .map_err(|e| HonchoError::Protocol(format!("parse chat response: {e}")))?;
        Ok(parsed.content)
    }
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct MessagesRequest {
    messages: Vec<MessageIn>,
}

#[derive(Debug, Serialize)]
struct MessageIn {
    peer_id: String,
    content: String,
    role: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    queries: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    content: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Env mutation in tests must be serialised to avoid races (Rust
    // tests run in parallel by default). The full suite runs with
    // --test-threads=1 but we belt-and-suspenders this anyway in case
    // someone runs `cargo test honcho` standalone.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn cfg() -> HonchoConfig {
        HonchoConfig {
            base_url: "http://localhost:9".to_string(),
            api_key: None,
            workspace_id: "ws".to_string(),
            timeout_secs: 1,
        }
    }

    // ---- HonchoConfig ----

    #[test]
    fn from_env_returns_none_when_base_url_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        // Save and clear all relevant vars.
        let saved: Vec<(String, Option<String>)> = [
            "HONCHO_BASE_URL",
            "HONCHO_API_KEY",
            "HONCHO_WORKSPACE_ID",
            "HONCHO_TIMEOUT_SECS",
        ]
        .iter()
        .map(|k| (k.to_string(), std::env::var(k).ok()))
        .collect();
        for (k, _) in &saved {
            std::env::remove_var(k);
        }
        let result = HonchoConfig::from_env();
        // Restore.
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
        assert!(result.is_none());
    }

    #[test]
    fn from_env_picks_up_all_fields() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved: Vec<(String, Option<String>)> = [
            "HONCHO_BASE_URL",
            "HONCHO_API_KEY",
            "HONCHO_WORKSPACE_ID",
            "HONCHO_TIMEOUT_SECS",
        ]
        .iter()
        .map(|k| (k.to_string(), std::env::var(k).ok()))
        .collect();
        std::env::set_var("HONCHO_BASE_URL", "https://h.example.com/v1/");
        std::env::set_var("HONCHO_API_KEY", "tok-123");
        std::env::set_var("HONCHO_WORKSPACE_ID", "my-ws");
        std::env::set_var("HONCHO_TIMEOUT_SECS", "42");
        let cfg = HonchoConfig::from_env().expect("Some cfg");
        // Restore.
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
        // Trailing slash trimmed.
        assert_eq!(cfg.base_url, "https://h.example.com/v1");
        assert_eq!(cfg.api_key.as_deref(), Some("tok-123"));
        assert_eq!(cfg.workspace_id, "my-ws");
        assert_eq!(cfg.timeout_secs, 42);
    }

    #[test]
    fn from_env_uses_defaults_when_optional_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved: Vec<(String, Option<String>)> = [
            "HONCHO_BASE_URL",
            "HONCHO_API_KEY",
            "HONCHO_WORKSPACE_ID",
            "HONCHO_TIMEOUT_SECS",
        ]
        .iter()
        .map(|k| (k.to_string(), std::env::var(k).ok()))
        .collect();
        std::env::set_var("HONCHO_BASE_URL", "http://localhost:8000/v1");
        std::env::remove_var("HONCHO_API_KEY");
        std::env::remove_var("HONCHO_WORKSPACE_ID");
        std::env::remove_var("HONCHO_TIMEOUT_SECS");
        let cfg = HonchoConfig::from_env().expect("Some cfg");
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
        assert_eq!(cfg.base_url, "http://localhost:8000/v1");
        assert_eq!(cfg.api_key, None);
        assert_eq!(cfg.workspace_id, "default");
        assert_eq!(cfg.timeout_secs, 10);
    }

    #[test]
    fn from_env_ignores_invalid_timeout() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved: Vec<(String, Option<String>)> = [
            "HONCHO_BASE_URL",
            "HONCHO_TIMEOUT_SECS",
        ]
        .iter()
        .map(|k| (k.to_string(), std::env::var(k).ok()))
        .collect();
        std::env::set_var("HONCHO_BASE_URL", "http://x/");
        std::env::set_var("HONCHO_TIMEOUT_SECS", "not-a-number");
        let cfg = HonchoConfig::from_env().expect("Some cfg");
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
        assert_eq!(cfg.timeout_secs, 10);
    }

    // ---- URL construction ----

    #[test]
    fn messages_url_joins_workspace_and_session() {
        let c = HonchoClient::new(cfg()).unwrap();
        assert_eq!(
            c.messages_url("sess-1"),
            "http://localhost:9/workspaces/ws/sessions/sess-1/messages"
        );
    }

    #[test]
    fn chat_url_joins_workspace_and_peer() {
        let c = HonchoClient::new(cfg()).unwrap();
        assert_eq!(
            c.chat_url("user-42"),
            "http://localhost:9/workspaces/ws/peers/user-42/chat"
        );
    }

    // ---- input validation ----

    #[tokio::test]
    async fn append_message_rejects_empty_session() {
        let c = HonchoClient::new(cfg()).unwrap();
        let err = c
            .append_message("", "p1", "hi", MessageRole::User)
            .await
            .unwrap_err();
        assert!(matches!(err, HonchoError::Config(_)), "{err:?}");
    }

    #[tokio::test]
    async fn append_message_rejects_empty_peer() {
        let c = HonchoClient::new(cfg()).unwrap();
        let err = c
            .append_message("s1", "", "hi", MessageRole::User)
            .await
            .unwrap_err();
        assert!(matches!(err, HonchoError::Config(_)), "{err:?}");
    }

    #[tokio::test]
    async fn dialectic_query_rejects_empty_query() {
        let c = HonchoClient::new(cfg()).unwrap();
        let err = c
            .dialectic_query("p1", "  \n", None)
            .await
            .unwrap_err();
        assert!(matches!(err, HonchoError::Config(_)), "{err:?}");
    }

    // ---- HTTP path with mock server ----

    /// Tiny inline HTTP/1.1 mock that accepts one connection, reads
    /// the request, and sends a fixed response. Returns the bound URL
    /// (no path, no trailing slash) and a join handle that yields the
    /// raw request bytes.
    async fn spawn_one_shot_mock(
        status_line: &'static str,
        response_body: String,
        content_type: &'static str,
    ) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        let body_bytes = response_body.into_bytes();
        let resp = format!(
            "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body_bytes.len()
        );
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
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
                        let headers = std::str::from_utf8(&buf[..pos]).unwrap_or("");
                        for line in headers.split("\r\n") {
                            if let Some(rest) = line
                                .to_ascii_lowercase()
                                .strip_prefix("content-length:")
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
    async fn append_message_sends_correct_body_and_url() {
        let (base, handle) = spawn_one_shot_mock(
            "HTTP/1.1 200 OK",
            "{}".to_string(),
            "application/json",
        )
        .await;
        let cfg = HonchoConfig {
            base_url: base.clone(),
            api_key: Some("secret-token".to_string()),
            workspace_id: "ws-prod".to_string(),
            timeout_secs: 5,
        };
        let client = HonchoClient::new(cfg).unwrap();
        client
            .append_message("sess-X", "user-7", "hello world", MessageRole::User)
            .await
            .unwrap();
        let req_bytes = handle.await.unwrap();
        let req_str = String::from_utf8_lossy(&req_bytes);
        // Path includes workspace + session.
        assert!(
            req_str.contains("POST /workspaces/ws-prod/sessions/sess-X/messages"),
            "request line missing expected path: {req_str}"
        );
        // Bearer auth header set.
        assert!(
            req_str.to_lowercase().contains("authorization: bearer secret-token"),
            "missing bearer token: {req_str}"
        );
        // Body contains expected JSON shape.
        assert!(req_str.contains(r#""peer_id":"user-7""#), "body: {req_str}");
        assert!(req_str.contains(r#""content":"hello world""#));
        assert!(req_str.contains(r#""role":"user""#));
    }

    #[tokio::test]
    async fn append_message_omits_auth_header_when_no_key() {
        let (base, handle) = spawn_one_shot_mock(
            "HTTP/1.1 200 OK",
            "{}".to_string(),
            "application/json",
        )
        .await;
        let cfg = HonchoConfig {
            base_url: base,
            api_key: None,
            workspace_id: "ws".to_string(),
            timeout_secs: 5,
        };
        let client = HonchoClient::new(cfg).unwrap();
        client
            .append_message("s", "p", "x", MessageRole::Assistant)
            .await
            .unwrap();
        let req_bytes = handle.await.unwrap();
        let req_str = String::from_utf8_lossy(&req_bytes);
        assert!(
            !req_str.to_lowercase().contains("authorization:"),
            "auth header should be absent: {req_str}"
        );
        assert!(req_str.contains(r#""role":"assistant""#));
    }

    #[tokio::test]
    async fn append_message_propagates_http_errors() {
        let (base, _handle) = spawn_one_shot_mock(
            "HTTP/1.1 503 Service Unavailable",
            r#"{"error":"down for maintenance"}"#.to_string(),
            "application/json",
        )
        .await;
        let cfg = HonchoConfig {
            base_url: base,
            api_key: None,
            workspace_id: "ws".to_string(),
            timeout_secs: 5,
        };
        let client = HonchoClient::new(cfg).unwrap();
        let err = client
            .append_message("s", "p", "x", MessageRole::User)
            .await
            .unwrap_err();
        match err {
            HonchoError::Http { status, body } => {
                assert_eq!(status, 503);
                assert!(body.contains("down for maintenance"), "body: {body}");
            }
            other => panic!("expected Http error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dialectic_query_returns_content_field() {
        let (base, handle) = spawn_one_shot_mock(
            "HTTP/1.1 200 OK",
            r#"{"content":"the user prefers terse answers"}"#.to_string(),
            "application/json",
        )
        .await;
        let cfg = HonchoConfig {
            base_url: base,
            api_key: None,
            workspace_id: "ws".to_string(),
            timeout_secs: 5,
        };
        let client = HonchoClient::new(cfg).unwrap();
        let answer = client
            .dialectic_query("user-9", "what does this user value?", Some("sess-A"))
            .await
            .unwrap();
        assert_eq!(answer, "the user prefers terse answers");
        let req_bytes = handle.await.unwrap();
        let req_str = String::from_utf8_lossy(&req_bytes);
        assert!(req_str.contains("/workspaces/ws/peers/user-9/chat"));
        assert!(req_str.contains(r#""session_id":"sess-A""#));
        assert!(req_str.contains(r#""stream":false"#));
        assert!(req_str.contains(r#""queries":["what does this user value?"]"#));
    }

    #[tokio::test]
    async fn dialectic_query_omits_session_when_none() {
        let (base, handle) = spawn_one_shot_mock(
            "HTTP/1.1 200 OK",
            r#"{"content":"ok"}"#.to_string(),
            "application/json",
        )
        .await;
        let cfg = HonchoConfig {
            base_url: base,
            api_key: None,
            workspace_id: "ws".to_string(),
            timeout_secs: 5,
        };
        let client = HonchoClient::new(cfg).unwrap();
        let _ = client.dialectic_query("p", "q", None).await.unwrap();
        let req_bytes = handle.await.unwrap();
        let req_str = String::from_utf8_lossy(&req_bytes);
        // session_id is `serde(skip_serializing_if = "Option::is_none")` —
        // omitted entirely when None.
        assert!(
            !req_str.contains(r#""session_id""#),
            "session_id should not be sent: {req_str}"
        );
    }

    #[tokio::test]
    async fn dialectic_query_protocol_error_on_missing_content_field() {
        let (base, _handle) = spawn_one_shot_mock(
            "HTTP/1.1 200 OK",
            r#"{"unexpected_field":"value"}"#.to_string(),
            "application/json",
        )
        .await;
        let cfg = HonchoConfig {
            base_url: base,
            api_key: None,
            workspace_id: "ws".to_string(),
            timeout_secs: 5,
        };
        let client = HonchoClient::new(cfg).unwrap();
        let err = client.dialectic_query("p", "q", None).await.unwrap_err();
        assert!(matches!(err, HonchoError::Protocol(_)), "{err:?}");
    }
}
