//! AWS Bedrock provider — Anthropic-on-Bedrock.
//!
//! Bedrock exposes Anthropic's Claude family at a different endpoint
//! shape than `api.anthropic.com`:
//!
//! - URL:  `POST https://bedrock-runtime.{region}.amazonaws.com/model/{model_id}/invoke`
//! - Auth: AWS Signature V4 over `bedrock` service in `{region}`
//!         (no `x-api-key`, no bearer token)
//! - Body: Anthropic Messages API JSON, but with two changes:
//!     * NO `model` field (model is in the URL path)
//!     * `anthropic_version: "bedrock-2023-05-31"` REQUIRED
//! - Errors: AWS error envelope (`{"message":"…"}` plus
//!   `x-amzn-ErrorType: …` header), not Anthropic's
//!   `{"error":{"message":"…"}}`
//!
//! The model ID in the URL is the Bedrock-side identifier
//! (e.g. `anthropic.claude-3-5-sonnet-20241022-v2:0`), not the
//! Anthropic API name (`claude-3-5-sonnet-20241022`). The provider
//! takes whatever model string the caller passes and URL-encodes it
//! into the path verbatim.
//!
//! ## Cross-account / IAM-role / SSO credentials
//!
//! Bedrock accepts long-lived (`AWS_ACCESS_KEY_ID` +
//! `AWS_SECRET_ACCESS_KEY`) and temporary (`AWS_SESSION_TOKEN`
//! additionally) credentials interchangeably. Temporary creds
//! must be refreshed externally — this provider doesn't talk to
//! STS or implement role assumption. The user's environment
//! (`aws sso login` / `aws assume-role` / IAM role on EC2) is
//! expected to keep the env vars fresh.
//!
//! ## Streaming
//!
//! Real streaming via `/invoke-with-response-stream` is wired in
//! through [`stream_wire::BedrockStream`]. Bedrock returns
//! `application/vnd.amazon.eventstream`-framed binary frames; each
//! frame's payload is a JSON object `{"bytes": "<base64>"}` whose
//! decoded payload is the same Anthropic SSE event JSON we already
//! parse. We reuse [`super::anthropic::wire::StreamConverter`] for
//! the event → [`StreamEvent`] state machine.
//!
//! ## What we deliberately don't do (yet)
//!
//! - IMDS / SSO token fetching — env vars only.
//! - Cross-region failover.
//! - Bedrock-Agent / Knowledge-Base APIs (those are different
//!   endpoints; this provider is for `bedrock-runtime` only).

use async_trait::async_trait;
use futures_util::stream::{BoxStream, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::anthropic::wire as anthropic_wire;
use crate::agent::llm::sigv4::{
    current_amz_date, sign, AwsCredentials, SignableRequest, SigningContext,
};
use crate::agent::llm::{
    ChatRequest, ChatResponse, LlmError, Provider, Result, StreamEvent,
};
use crate::config::AgentConfig;

pub const PROVIDER_NAME: &str = "bedrock";

/// Bedrock Anthropic protocol pin. Bedrock runtime requires this
/// exact string for Anthropic models — bumping it without coordinating
/// with AWS will yield 400s. See:
/// <https://docs.aws.amazon.com/bedrock/latest/userguide/model-parameters-anthropic-claude-messages.html>
pub const BEDROCK_ANTHROPIC_VERSION: &str = "bedrock-2023-05-31";

/// AWS service name in the SigV4 credential scope.
pub const BEDROCK_SERVICE: &str = "bedrock";

/// Default region used when the agent didn't override it. `us-east-1`
/// matches the AWS SDK convention and has the broadest model
/// availability.
pub const DEFAULT_REGION: &str = "us-east-1";

/// Default env vars per AWS SDK convention.
pub const DEFAULT_ACCESS_KEY_ENV: &str = "AWS_ACCESS_KEY_ID";
pub const DEFAULT_SECRET_KEY_ENV: &str = "AWS_SECRET_ACCESS_KEY";
pub const DEFAULT_SESSION_TOKEN_ENV: &str = "AWS_SESSION_TOKEN";

#[derive(Clone)]
pub struct BedrockConfig {
    pub region: String,
    /// Endpoint base. Empty string falls back to the region-derived
    /// default (`https://bedrock-runtime.<region>.amazonaws.com`).
    /// Override is rare — only useful for VPC endpoints / FIPS
    /// endpoints / mocks in tests.
    pub base_url: Option<String>,
    pub model: String,
    pub credentials: Option<AwsCredentials>,
    pub extra_headers: HashMap<String, String>,
    pub request_timeout: Duration,
}

impl std::fmt::Debug for BedrockConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BedrockConfig")
            .field("region", &self.region)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field(
                "credentials_present",
                &self.credentials.as_ref().is_some(),
            )
            .field(
                "session_token_present",
                &self
                    .credentials
                    .as_ref()
                    .is_some_and(|c| c.session_token.is_some()),
            )
            .field("extra_headers", &self.extra_headers)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl BedrockConfig {
    pub fn from_agent_config(model: &str, agent: &AgentConfig) -> Self {
        let region = agent
            .aws_region
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_REGION.to_string());

        let base_url = agent.base_url.clone().filter(|s| !s.is_empty());

        let credentials = resolve_credentials(agent);

        let request_timeout = if agent.request_timeout == 0 {
            Duration::from_secs(0)
        } else {
            Duration::from_secs(agent.request_timeout)
        };

        Self {
            region,
            base_url,
            model: model.to_string(),
            credentials,
            extra_headers: agent.extra_headers.clone(),
            request_timeout,
        }
    }

    /// Region-derived host for SigV4 signing AND the URL we POST to.
    /// Returns the host portion only (no scheme), because the SigV4
    /// canonical headers include `host:` without scheme/port.
    fn host(&self) -> String {
        if let Some(url) = &self.base_url {
            // Strip scheme + path so the canonical host header matches
            // what reqwest writes on the wire.
            host_from_url(url).unwrap_or_else(|| {
                format!("bedrock-runtime.{}.amazonaws.com", self.region)
            })
        } else {
            format!("bedrock-runtime.{}.amazonaws.com", self.region)
        }
    }

    fn endpoint_base(&self) -> String {
        self.base_url
            .clone()
            .unwrap_or_else(|| format!("https://bedrock-runtime.{}.amazonaws.com", self.region))
            .trim_end_matches('/')
            .to_string()
    }
}

/// Extract the host portion of a URL without depending on `url` crate.
/// Handles `https://host[:port][/...]` and `http://...`. Returns
/// `None` for malformed input — caller falls back to the regional
/// default.
fn host_from_url(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host_with_port = after_scheme.split('/').next().unwrap_or(after_scheme);
    if host_with_port.is_empty() {
        return None;
    }
    Some(host_with_port.to_string())
}

/// Look up access key + secret + optional session token from the
/// agent config. Returns `None` if access key OR secret key is
/// missing — partial credentials are useless and we don't want to
/// silently send an unsigned request.
fn resolve_credentials(agent: &AgentConfig) -> Option<AwsCredentials> {
    let access_key = lookup_aws_value(
        agent.aws_access_key_credential.as_deref(),
        agent.aws_access_key_env.as_deref(),
        DEFAULT_ACCESS_KEY_ENV,
    )?;
    let secret_key = lookup_aws_value(
        agent.aws_secret_key_credential.as_deref(),
        agent.aws_secret_key_env.as_deref(),
        DEFAULT_SECRET_KEY_ENV,
    )?;
    let session_token = lookup_aws_value(
        agent.aws_session_token_credential.as_deref(),
        agent.aws_session_token_env.as_deref(),
        DEFAULT_SESSION_TOKEN_ENV,
    );

    let mut creds = AwsCredentials::new(access_key, secret_key);
    if let Some(t) = session_token {
        creds = creds.with_session_token(t);
    }
    Some(creds)
}

/// Resolve a single AWS-credential value with the standard
/// `*_credential` (cos credential store) → `*_env` (override) →
/// `<default-env>` (AWS SDK convention) ladder.
fn lookup_aws_value(
    cred_name: Option<&str>,
    env_name: Option<&str>,
    default_env: &str,
) -> Option<String> {
    if let Some(name) = cred_name {
        if let Ok(Some(v)) = crate::credential::try_load(name, "agent") {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    let env = env_name.unwrap_or(default_env);
    if let Ok(v) = std::env::var(env) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    None
}

pub struct BedrockProvider {
    cfg: BedrockConfig,
    client: reqwest::Client,
}

impl BedrockProvider {
    pub fn new(cfg: BedrockConfig) -> Self {
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("cos-agent/", env!("CARGO_PKG_VERSION")));
        if cfg.request_timeout > Duration::from_secs(0) {
            builder = builder.timeout(cfg.request_timeout);
        }
        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        Self { cfg, client }
    }

    pub fn from_agent_config(model: &str, agent: &AgentConfig) -> Self {
        Self::new(BedrockConfig::from_agent_config(model, agent))
    }

    /// `/model/<url-encoded model id>/invoke` — exact path, before SigV4
    /// canonicalization (which double-encodes per non-S3 rules).
    fn model_path(&self) -> String {
        format!("/model/{}/invoke", url_encode_path_segment(&self.cfg.model))
    }

    fn stream_model_path(&self) -> String {
        format!(
            "/model/{}/invoke-with-response-stream",
            url_encode_path_segment(&self.cfg.model)
        )
    }

    fn stream_full_url(&self) -> String {
        format!("{}{}", self.cfg.endpoint_base(), self.stream_model_path())
    }

    fn full_url(&self) -> String {
        format!("{}{}", self.cfg.endpoint_base(), self.model_path())
    }
}

/// Percent-encode a single URL path segment per RFC 3986
/// unreserved set. Bedrock model IDs contain `:` (`v2:0`),
/// `.` (`anthropic.claude-3-…`) and `-`, so we must encode `:` but
/// leave `.` and `-` alone.
fn url_encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for byte in s.bytes() {
        let unreserved = byte.is_ascii_alphanumeric()
            || byte == b'-'
            || byte == b'_'
            || byte == b'.'
            || byte == b'~';
        if unreserved {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

#[async_trait]
impl Provider for BedrockProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn supported_models(&self) -> Vec<String> {
        vec![self.cfg.model.clone()]
    }

    fn is_configured(&self) -> bool {
        self.cfg.credentials.is_some()
    }

    fn supports_prompt_cache(&self) -> bool {
        // Bedrock-side Anthropic models accept the same
        // `cache_control: ephemeral` markers — propagate the
        // capability so prompt caching turns on for cached prompts.
        true
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let creds = self.cfg.credentials.as_ref().ok_or_else(|| {
            LlmError::NotConfigured(
                "bedrock: missing AWS credentials (set AWS_ACCESS_KEY_ID + \
                 AWS_SECRET_ACCESS_KEY env vars or aws_*_credential / aws_*_env \
                 fields in [agent])"
                    .into(),
            )
        })?;

        let body_bytes = build_bedrock_body_bytes(&request)?;

        let amz_date = current_amz_date()
            .map_err(|e| LlmError::InvalidRequest(format!("clock: {e}")))?;
        let ctx = SigningContext {
            region: self.cfg.region.clone(),
            service: BEDROCK_SERVICE.to_string(),
            amz_date,
        };
        let host = self.cfg.host();
        let path = self.model_path();
        let signable = SignableRequest {
            method: "POST",
            path: &path,
            query: &[],
            // We pass an explicit content-type so it's part of the
            // signature scope — any tampering at the proxy layer
            // would invalidate the signature.
            headers: &[(
                "content-type".to_string(),
                "application/json".to_string(),
            )],
            body: &body_bytes,
        };
        let signed = sign(creds, &ctx, &host, &signable);

        let mut http = self
            .client
            .post(self.full_url())
            .header("Content-Type", "application/json")
            // accept identifies us in CloudTrail logs
            .header("Accept", "application/json")
            .body(body_bytes);

        for (name, value) in signed.as_header_pairs() {
            http = http.header(name, value);
        }
        for (k, v) in &self.cfg.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }

        let resp = http.send().await.map_err(LlmError::Transport)?;
        let status = resp.status();
        let retry_after_secs = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        let amz_error_type = resp
            .headers()
            .get("x-amzn-errortype")
            .or_else(|| resp.headers().get("X-Amzn-Errortype"))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let bytes = resp.bytes().await.map_err(LlmError::Transport)?;

        if !status.is_success() {
            return Err(classify_bedrock_error(
                status,
                &bytes,
                amz_error_type.as_deref(),
                retry_after_secs,
            ));
        }

        // Body shape is identical to Anthropic's Messages API
        // response — reuse the parser.
        let parsed: anthropic_wire::Response = serde_json::from_slice(&bytes)
            .map_err(|e| LlmError::Parse(e.to_string()))?;
        anthropic_wire::response_to_chat(parsed, &self.cfg.model)
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let creds = self.cfg.credentials.as_ref().ok_or_else(|| {
            LlmError::NotConfigured(
                "bedrock: missing AWS credentials (set AWS_ACCESS_KEY_ID + \
                 AWS_SECRET_ACCESS_KEY env vars or aws_*_credential / aws_*_env \
                 fields in [agent])"
                    .into(),
            )
        })?;

        let body_bytes = build_bedrock_body_bytes(&request)?;

        let amz_date = current_amz_date()
            .map_err(|e| LlmError::InvalidRequest(format!("clock: {e}")))?;
        let ctx = SigningContext {
            region: self.cfg.region.clone(),
            service: BEDROCK_SERVICE.to_string(),
            amz_date,
        };
        let host = self.cfg.host();
        let path = self.stream_model_path();
        // Note: per Bedrock docs the `accept` for streamed responses
        // is `application/vnd.amazon.eventstream`, expressed via the
        // `X-Amzn-Bedrock-Accept` header rather than the normal
        // `Accept` header. We sign content-type only — the bedrock
        // accept header is informational and not part of the SigV4
        // canonical headers.
        let signable = SignableRequest {
            method: "POST",
            path: &path,
            query: &[],
            headers: &[(
                "content-type".to_string(),
                "application/json".to_string(),
            )],
            body: &body_bytes,
        };
        let signed = sign(creds, &ctx, &host, &signable);

        let mut http = self
            .client
            .post(self.stream_full_url())
            .header("Content-Type", "application/json")
            .header(
                "X-Amzn-Bedrock-Accept",
                "application/vnd.amazon.eventstream",
            )
            .body(body_bytes);

        for (name, value) in signed.as_header_pairs() {
            http = http.header(name, value);
        }
        for (k, v) in &self.cfg.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }

        let resp = http.send().await.map_err(LlmError::Transport)?;
        let status = resp.status();
        let retry_after_secs = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        let amz_error_type = resp
            .headers()
            .get("x-amzn-errortype")
            .or_else(|| resp.headers().get("X-Amzn-Errortype"))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        if !status.is_success() {
            // Pre-stream HTTP error (validation, auth, throttling
            // before model engagement, ModelNotReadyException, etc).
            // Body is small JSON; read it synchronously so the error
            // we surface includes the AWS message.
            let bytes = resp.bytes().await.map_err(LlmError::Transport)?;
            return Err(classify_bedrock_error(
                status,
                &bytes,
                amz_error_type.as_deref(),
                retry_after_secs,
            ));
        }

        let bytes_stream = resp.bytes_stream();
        let stream = stream_wire::BedrockStream::new(bytes_stream, &self.cfg.model);
        Ok(stream.boxed())
    }
}

/// Build the JSON body for Bedrock invoke. Internally this calls
/// the existing Anthropic body builder (so prompt caching, tool
/// use, system prompt hoisting all work the same), then performs
/// the two Bedrock-specific edits:
///
///   1. Strip the `model` field — Bedrock takes it from the URL.
///   2. Insert `anthropic_version: "bedrock-2023-05-31"` —
///      required by the Bedrock runtime.
fn build_bedrock_body_bytes(request: &ChatRequest) -> Result<Vec<u8>> {
    // The model name we pass in here is irrelevant — we strip it.
    let mut body = anthropic_wire::build_request_body(request, "_unused_", false);
    if let Some(obj) = body.as_object_mut() {
        obj.remove("model");
        obj.insert(
            "anthropic_version".into(),
            serde_json::Value::String(BEDROCK_ANTHROPIC_VERSION.to_string()),
        );
    } else {
        return Err(LlmError::InvalidRequest(
            "anthropic body builder did not produce a JSON object".into(),
        ));
    }
    serde_json::to_vec(&body).map_err(|e| LlmError::Parse(e.to_string()))
}

/// Map a Bedrock non-2xx HTTP response into an [`LlmError`].
///
/// Bedrock surfaces machine-readable error types in the
/// `x-amzn-ErrorType` response header (e.g. `ThrottlingException`,
/// `ValidationException`, `ResourceNotFoundException`). When
/// present, prefer those over the body's `message` field —
/// they're stable and parseable.
fn classify_bedrock_error(
    status: reqwest::StatusCode,
    body: &[u8],
    amz_error_type: Option<&str>,
    retry_after_secs: Option<u64>,
) -> LlmError {
    let body_text = String::from_utf8_lossy(body).to_string();
    let upstream_message = extract_aws_error_message(&body_text)
        .unwrap_or_else(|| body_text.chars().take(500).collect::<String>());

    // Specific AWS error types take precedence over status code.
    if let Some(t) = amz_error_type {
        // The header may be `Type:Hash` — split on `:` to normalise.
        let core = t.split(':').next().unwrap_or(t);
        match core {
            "ThrottlingException"
            | "TooManyRequestsException"
            | "ServiceQuotaExceededException" => {
                return LlmError::RateLimited {
                    retry_after_ms: retry_after_secs
                        .map(|s| s.saturating_mul(1_000))
                        .unwrap_or(1_000),
                };
            }
            "AccessDeniedException"
            | "UnauthorizedException"
            | "MissingAuthenticationTokenException"
            | "InvalidSignatureException"
            | "ExpiredTokenException" => {
                return LlmError::Auth;
            }
            _ => {}
        }
    }

    match status.as_u16() {
        401 | 403 => LlmError::Auth,
        429 => LlmError::RateLimited {
            retry_after_ms: retry_after_secs
                .map(|s| s.saturating_mul(1_000))
                .unwrap_or(1_000),
        },
        _ => LlmError::Provider {
            status: status.as_u16(),
            message: upstream_message,
        },
    }
}

/// AWS error envelope: `{"message":"…","__type":"…"}`. Some services
/// also use `{"Message":"…"}` (capitalised). Try both.
fn extract_aws_error_message(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if let Some(s) = v.get("message").and_then(|m| m.as_str()) {
        return Some(s.to_string());
    }
    if let Some(s) = v.get("Message").and_then(|m| m.as_str()) {
        return Some(s.to_string());
    }
    if let Some(s) = v.get("error").and_then(|e| e.as_str()) {
        return Some(s.to_string());
    }
    None
}

pub fn is_alias(name: &str) -> bool {
    name == PROVIDER_NAME
}

pub fn build_provider(model: &str, agent: &AgentConfig) -> Arc<dyn Provider> {
    Arc::new(BedrockProvider::from_agent_config(model, agent))
}

// --------------------------------------------------------------------
// Streaming wire layer.
//
// Bedrock's `/invoke-with-response-stream` returns an HTTP body
// framed with the AWS EventStream binary protocol. Each frame is one
// of three kinds, dispatched on the `:message-type` header:
//
//   - `event`     → normal event; `:event-type=chunk` carries
//                   `{"bytes": "<base64>"}` whose decoded payload is
//                   the same JSON we'd see on Anthropic's SSE stream
//                   (`message_start`, `content_block_delta`, etc).
//   - `exception` → modeled error from the streaming union. The
//                   `:exception-type` header carries the Smithy
//                   union-member name in lower-camelCase
//                   (`throttlingException`, `validationException`, …).
//                   We accept PascalCase as a defensive fallback.
//   - `error`     → unmodeled error envelope. `:error-code` /
//                   `:error-message` headers carry detail.
//
// Note that `ModelNotReadyException` is a *pre-stream* HTTP-level
// error (409), not a streamed exception, so it's handled by the
// `chat_stream` HTTP-status branch above, not here.
//
// We reuse the Anthropic SSE state machine
// (`anthropic_wire::StreamConverter`) by synthesising synthetic SSE
// events (`event:` field = inner event type, `data:` field = inner
// JSON string) from the inner chunk payload.
pub(crate) mod stream_wire {
    use super::anthropic_wire;
    use crate::agent::llm::aws_eventstream::{
        EventStreamParser, Frame, FrameError,
    };
    use crate::agent::llm::sse::SseEvent;
    use crate::agent::llm::{LlmError, Result, StreamEvent};
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use bytes::Bytes;
    use futures_util::Stream;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// Streaming wrapper that demuxes AWS EventStream frames and
    /// forwards them through the Anthropic SSE converter.
    ///
    /// Generic over the byte source (any
    /// `Stream<Item=Result<Bytes, reqwest::Error>>`) so unit tests
    /// can drive it with a `stream::iter([...])` of synthetic
    /// frames.
    pub(crate) struct BedrockStream<S> {
        inner: S,
        parser: EventStreamParser,
        converter: anthropic_wire::StreamConverter,
        /// Events ready to yield to the caller before pulling more
        /// bytes.
        pending: VecDeque<Result<StreamEvent>>,
        /// Once set, no further events are emitted; the next poll
        /// returns `None` (terminator).
        done: bool,
        /// Set when the byte source emits its EOF or its own error.
        bytes_done: bool,
    }

    impl<S> BedrockStream<S>
    where
        S: Stream<Item = std::result::Result<Bytes, reqwest::Error>>
            + Send
            + 'static,
    {
        pub(crate) fn new(inner: S, default_model: &str) -> Self {
            Self {
                inner,
                parser: EventStreamParser::new(),
                converter: anthropic_wire::StreamConverter::new(default_model),
                pending: VecDeque::new(),
                done: false,
                bytes_done: false,
            }
        }

        /// Drain frames currently available in the parser and feed
        /// them to the converter, queueing any resulting events.
        fn drain_frames(&mut self) {
            while let Some(frame) = self.parser.pop_frame() {
                match frame {
                    Ok(f) => self.handle_frame(f),
                    Err(e) => {
                        self.pending.push_back(Err(LlmError::Stream(format!(
                            "bedrock event stream framing: {e}"
                        ))));
                        self.done = true;
                        return;
                    }
                }
                if self.done {
                    return;
                }
            }
        }

        fn handle_frame(&mut self, frame: Frame) {
            let msg_type = frame
                .headers
                .get(":message-type")
                .map(|s| s.as_str())
                .unwrap_or("");
            match msg_type {
                "event" => self.handle_event_frame(frame),
                "exception" => self.handle_exception_frame(frame),
                "error" => self.handle_error_frame(frame),
                "" => {
                    // No `:message-type` header is malformed —
                    // surface as a stream error so the caller sees
                    // it instead of stalling.
                    self.pending.push_back(Err(LlmError::Stream(
                        "bedrock frame missing :message-type header".into(),
                    )));
                    self.done = true;
                }
                other => {
                    // Unknown message-type. Per forward-compat
                    // policy, ignore quietly — AWS may add new
                    // categories. Still log to stderr at debug.
                    tracing::debug!(
                        target: "cos::bedrock_stream",
                        "ignoring unknown :message-type {other:?}"
                    );
                }
            }
        }

        fn handle_event_frame(&mut self, frame: Frame) {
            let ev_type = frame
                .headers
                .get(":event-type")
                .map(|s| s.as_str())
                .unwrap_or("");
            // The only event-type that carries chat content is
            // `chunk`. Other event types (e.g. `metadata`) we ignore.
            if ev_type != "chunk" {
                return;
            }

            // chunk payload is `{"bytes": "<base64>"}`. base64 of
            // the inner Anthropic SSE event JSON.
            let outer: serde_json::Value =
                match serde_json::from_slice(&frame.payload) {
                    Ok(v) => v,
                    Err(e) => {
                        self.pending.push_back(Err(LlmError::Parse(format!(
                            "bedrock chunk frame: outer json: {e}"
                        ))));
                        self.done = true;
                        return;
                    }
                };
            let b64 = match outer.get("bytes").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => {
                    self.pending.push_back(Err(LlmError::Parse(
                        "bedrock chunk frame missing 'bytes' field".into(),
                    )));
                    self.done = true;
                    return;
                }
            };
            let inner_bytes = match B64.decode(b64) {
                Ok(b) => b,
                Err(e) => {
                    self.pending.push_back(Err(LlmError::Parse(format!(
                        "bedrock chunk frame: base64: {e}"
                    ))));
                    self.done = true;
                    return;
                }
            };
            let inner_json = match std::str::from_utf8(&inner_bytes) {
                Ok(s) => s,
                Err(e) => {
                    self.pending.push_back(Err(LlmError::Parse(format!(
                        "bedrock chunk frame: utf8: {e}"
                    ))));
                    self.done = true;
                    return;
                }
            };

            // Read inner event type from JSON itself — Bedrock does
            // not pass the SSE `event:` line, only the JSON. The
            // converter already supports falling back to
            // `payload.type` when `ev.event` is empty, but we'll
            // populate it for clarity.
            let parsed: serde_json::Value = match serde_json::from_str(inner_json) {
                Ok(v) => v,
                Err(e) => {
                    self.pending.push_back(Err(LlmError::Parse(format!(
                        "bedrock chunk frame: inner json: {e}"
                    ))));
                    self.done = true;
                    return;
                }
            };
            let ev_kind = parsed
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let synth = SseEvent {
                event: ev_kind,
                data: inner_json.to_string(),
            };
            for result in self.converter.process(&synth) {
                self.pending.push_back(result);
            }
            if self.converter.is_finished() {
                self.done = true;
            }
        }

        fn handle_exception_frame(&mut self, frame: Frame) {
            let exception_type = frame
                .headers
                .get(":exception-type")
                .cloned()
                .unwrap_or_default();
            // Try to extract a message from the JSON payload (most
            // exceptions carry `{"message":"…"}`).
            let payload_msg = std::str::from_utf8(&frame.payload)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|v| {
                    v.get("message")
                        .or_else(|| v.get("Message"))
                        .and_then(|m| m.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();

            let err = classify_streamed_exception(&exception_type, &payload_msg);
            self.pending.push_back(Err(err));
            self.done = true;
        }

        fn handle_error_frame(&mut self, frame: Frame) {
            let code = frame
                .headers
                .get(":error-code")
                .cloned()
                .unwrap_or_default();
            let message = frame
                .headers
                .get(":error-message")
                .cloned()
                .unwrap_or_else(|| {
                    std::str::from_utf8(&frame.payload)
                        .unwrap_or("")
                        .to_string()
                });
            let summary = if code.is_empty() {
                format!("bedrock streamed unmodeled error: {message}")
            } else {
                format!("bedrock streamed error [{code}]: {message}")
            };
            self.pending.push_back(Err(LlmError::Provider {
                status: 500,
                message: summary,
            }));
            self.done = true;
        }
    }

    impl<S> Stream for BedrockStream<S>
    where
        S: Stream<Item = std::result::Result<Bytes, reqwest::Error>>
            + Send
            + Unpin
            + 'static,
    {
        type Item = Result<StreamEvent>;

        fn poll_next(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Self::Item>> {
            loop {
                if let Some(item) = self.pending.pop_front() {
                    return Poll::Ready(Some(item));
                }
                if self.done {
                    return Poll::Ready(None);
                }
                if self.bytes_done {
                    // Bytes source is finished; check the parser for
                    // truncation, then terminate.
                    let p = std::mem::take(&mut self.parser);
                    if let Err(e) = p.finish() {
                        match e {
                            FrameError::Truncated(n) => {
                                self.pending.push_back(Err(LlmError::Stream(
                                    format!(
                                        "bedrock event stream truncated: {n} \
                                         byte(s) of partial frame at EOF"
                                    ),
                                )));
                            }
                            other => {
                                self.pending.push_back(Err(LlmError::Stream(
                                    format!(
                                        "bedrock event stream framing: {other}"
                                    ),
                                )));
                            }
                        }
                    }
                    self.done = true;
                    continue;
                }

                match Pin::new(&mut self.inner).poll_next(cx) {
                    Poll::Ready(Some(Ok(chunk))) => {
                        self.parser.feed(&chunk);
                        self.drain_frames();
                    }
                    Poll::Ready(Some(Err(e))) => {
                        self.pending
                            .push_back(Err(LlmError::Transport(e)));
                        self.done = true;
                    }
                    Poll::Ready(None) => {
                        self.bytes_done = true;
                    }
                    Poll::Pending => return Poll::Pending,
                }
            }
        }
    }

    /// Map a streamed exception name (from `:exception-type` header)
    /// to an [`LlmError`]. Bedrock streams exception names as Smithy
    /// union member names in **lower-camelCase**
    /// (`throttlingException`); some clients pass PascalCase, so
    /// this also accepts that form defensively.
    pub(crate) fn classify_streamed_exception(name: &str, message: &str) -> LlmError {
        // Normalise to lowercase first letter for matching.
        let normalised = lower_camel(name);
        match normalised.as_str() {
            "throttlingException" => LlmError::RateLimited {
                retry_after_ms: 0,
            },
            "validationException" => {
                let m = if message.is_empty() {
                    "bedrock streamed validationException".into()
                } else {
                    format!("bedrock validationException: {message}")
                };
                LlmError::InvalidRequest(m)
            }
            "modelStreamErrorException" => LlmError::Provider {
                status: 500,
                message: format!(
                    "bedrock modelStreamErrorException: {}",
                    if message.is_empty() {
                        "model produced unrecoverable stream error"
                    } else {
                        message
                    }
                ),
            },
            "modelTimeoutException" => LlmError::Provider {
                status: 504,
                message: format!(
                    "bedrock modelTimeoutException: {}",
                    if message.is_empty() {
                        "model failed to respond within timeout"
                    } else {
                        message
                    }
                ),
            },
            "internalServerException" => LlmError::Provider {
                status: 500,
                message: format!(
                    "bedrock internalServerException: {}",
                    if message.is_empty() {
                        "internal server error"
                    } else {
                        message
                    }
                ),
            },
            "serviceUnavailableException" => LlmError::Provider {
                status: 503,
                message: format!(
                    "bedrock serviceUnavailableException: {}",
                    if message.is_empty() {
                        "service unavailable"
                    } else {
                        message
                    }
                ),
            },
            other => {
                // Unknown exception name. Don't swallow — surface
                // as a generic provider error so the caller can
                // log it.
                LlmError::Provider {
                    status: 500,
                    message: format!(
                        "bedrock unknown streamed exception {other:?}: {}",
                        if message.is_empty() {
                            "(no message)"
                        } else {
                            message
                        }
                    ),
                }
            }
        }
    }

    fn lower_camel(s: &str) -> String {
        let mut chars = s.chars();
        match chars.next() {
            Some(c) => {
                let mut out = String::with_capacity(s.len());
                for lower_c in c.to_lowercase() {
                    out.push(lower_c);
                }
                out.push_str(chars.as_str());
                out
            }
            None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{Message, ToolChoice};

    fn cfg() -> AgentConfig {
        AgentConfig::default()
    }

    fn req_text(text: &str) -> ChatRequest {
        ChatRequest {
            model: "anthropic.claude-3-5-sonnet-20241022-v2:0".into(),
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

    // ---- Region resolution ----------------------------------------------

    #[test]
    fn region_defaults_to_us_east_1() {
        let bc = BedrockConfig::from_agent_config("foo", &cfg());
        assert_eq!(bc.region, "us-east-1");
    }

    #[test]
    fn region_override_takes_effect() {
        let mut c = cfg();
        c.aws_region = Some("eu-west-1".into());
        let bc = BedrockConfig::from_agent_config("foo", &c);
        assert_eq!(bc.region, "eu-west-1");
    }

    #[test]
    fn empty_region_falls_back_to_default() {
        let mut c = cfg();
        c.aws_region = Some(String::new());
        let bc = BedrockConfig::from_agent_config("foo", &c);
        assert_eq!(bc.region, "us-east-1");
    }

    // ---- Endpoint ---------------------------------------------------------

    #[test]
    fn host_uses_region_default() {
        let mut c = cfg();
        c.aws_region = Some("ap-southeast-2".into());
        let bc = BedrockConfig::from_agent_config("foo", &c);
        assert_eq!(bc.host(), "bedrock-runtime.ap-southeast-2.amazonaws.com");
    }

    #[test]
    fn host_uses_base_url_override_when_set() {
        let mut c = cfg();
        c.base_url = Some("https://my-vpc-endpoint.example/bedrock".into());
        let bc = BedrockConfig::from_agent_config("foo", &c);
        assert_eq!(bc.host(), "my-vpc-endpoint.example");
    }

    #[test]
    fn endpoint_base_is_region_derived() {
        let bc = BedrockConfig::from_agent_config("foo", &cfg());
        assert_eq!(
            bc.endpoint_base(),
            "https://bedrock-runtime.us-east-1.amazonaws.com"
        );
    }

    #[test]
    fn endpoint_base_strips_trailing_slash() {
        let mut c = cfg();
        c.base_url = Some("https://my.proxy/".into());
        let bc = BedrockConfig::from_agent_config("foo", &c);
        assert_eq!(bc.endpoint_base(), "https://my.proxy");
    }

    // ---- Model path encoding ----------------------------------------------

    #[test]
    fn model_path_encodes_colon() {
        let mut c = cfg();
        c.aws_access_key_env = Some("COS_BR_AK_TEST_X".into());
        c.aws_secret_key_env = Some("COS_BR_SK_TEST_X".into());
        std::env::set_var("COS_BR_AK_TEST_X", "AKID");
        std::env::set_var("COS_BR_SK_TEST_X", "secret");
        let p = BedrockProvider::from_agent_config(
            "anthropic.claude-3-5-sonnet-20241022-v2:0",
            &c,
        );
        // : → %3A, dot stays, dash stays.
        assert_eq!(
            p.model_path(),
            "/model/anthropic.claude-3-5-sonnet-20241022-v2%3A0/invoke"
        );
        std::env::remove_var("COS_BR_AK_TEST_X");
        std::env::remove_var("COS_BR_SK_TEST_X");
    }

    #[test]
    fn full_url_combines_base_and_model_path() {
        let mut c = cfg();
        c.aws_region = Some("us-west-2".into());
        c.aws_access_key_env = Some("COS_BR_AK_TEST_Y".into());
        c.aws_secret_key_env = Some("COS_BR_SK_TEST_Y".into());
        std::env::set_var("COS_BR_AK_TEST_Y", "AKID");
        std::env::set_var("COS_BR_SK_TEST_Y", "secret");
        let p = BedrockProvider::from_agent_config("anthropic.claude-foo", &c);
        assert_eq!(
            p.full_url(),
            "https://bedrock-runtime.us-west-2.amazonaws.com/model/anthropic.claude-foo/invoke"
        );
        std::env::remove_var("COS_BR_AK_TEST_Y");
        std::env::remove_var("COS_BR_SK_TEST_Y");
    }

    // ---- Credential resolution -------------------------------------------

    #[test]
    fn is_configured_false_without_credentials() {
        // Default env doesn't have AWS_ACCESS_KEY_ID / SECRET set
        // (or even if it did, the test isolates with custom names).
        let mut c = cfg();
        c.aws_access_key_env = Some("COS_BR_NOSUCH_AK".into());
        c.aws_secret_key_env = Some("COS_BR_NOSUCH_SK".into());
        let p = BedrockProvider::from_agent_config("foo", &c);
        assert!(!p.is_configured());
    }

    #[test]
    fn is_configured_true_with_env_credentials() {
        let mut c = cfg();
        c.aws_access_key_env = Some("COS_BR_AK_TEST_Z".into());
        c.aws_secret_key_env = Some("COS_BR_SK_TEST_Z".into());
        std::env::set_var("COS_BR_AK_TEST_Z", "AKIDFAKE");
        std::env::set_var("COS_BR_SK_TEST_Z", "secret");
        let p = BedrockProvider::from_agent_config("foo", &c);
        assert!(p.is_configured());
        std::env::remove_var("COS_BR_AK_TEST_Z");
        std::env::remove_var("COS_BR_SK_TEST_Z");
    }

    #[test]
    fn missing_secret_disables_provider() {
        let mut c = cfg();
        c.aws_access_key_env = Some("COS_BR_AK_TEST_W".into());
        c.aws_secret_key_env = Some("COS_BR_NOSUCH_SK_W".into());
        std::env::set_var("COS_BR_AK_TEST_W", "AKIDFAKE");
        let p = BedrockProvider::from_agent_config("foo", &c);
        assert!(!p.is_configured(), "access-key only must NOT be configured");
        std::env::remove_var("COS_BR_AK_TEST_W");
    }

    #[test]
    fn empty_credential_value_is_ignored() {
        let mut c = cfg();
        c.aws_access_key_env = Some("COS_BR_AK_TEST_EMPTY".into());
        c.aws_secret_key_env = Some("COS_BR_SK_TEST_EMPTY".into());
        std::env::set_var("COS_BR_AK_TEST_EMPTY", "");
        std::env::set_var("COS_BR_SK_TEST_EMPTY", "");
        let p = BedrockProvider::from_agent_config("foo", &c);
        assert!(!p.is_configured());
        std::env::remove_var("COS_BR_AK_TEST_EMPTY");
        std::env::remove_var("COS_BR_SK_TEST_EMPTY");
    }

    #[test]
    fn session_token_is_optional_and_picked_up_when_present() {
        let mut c = cfg();
        c.aws_access_key_env = Some("COS_BR_AK_TEST_S".into());
        c.aws_secret_key_env = Some("COS_BR_SK_TEST_S".into());
        c.aws_session_token_env = Some("COS_BR_ST_TEST_S".into());
        std::env::set_var("COS_BR_AK_TEST_S", "AKID");
        std::env::set_var("COS_BR_SK_TEST_S", "secret");
        std::env::set_var("COS_BR_ST_TEST_S", "FwoG-fake-token");
        let bc = BedrockConfig::from_agent_config("foo", &c);
        let creds = bc.credentials.expect("creds resolved");
        assert_eq!(creds.session_token.as_deref(), Some("FwoG-fake-token"));
        std::env::remove_var("COS_BR_AK_TEST_S");
        std::env::remove_var("COS_BR_SK_TEST_S");
        std::env::remove_var("COS_BR_ST_TEST_S");
    }

    // ---- Body building ----------------------------------------------------

    #[test]
    fn body_strips_model_field() {
        let body_bytes = build_bedrock_body_bytes(&req_text("hello")).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert!(v.get("model").is_none(), "model field must be stripped");
    }

    #[test]
    fn body_includes_bedrock_anthropic_version() {
        let body_bytes = build_bedrock_body_bytes(&req_text("hello")).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(
            v.get("anthropic_version").and_then(|x| x.as_str()),
            Some("bedrock-2023-05-31")
        );
    }

    #[test]
    fn body_keeps_anthropic_messages_shape() {
        let body_bytes = build_bedrock_body_bytes(&req_text("hello")).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        // System hoisted to top-level (Anthropic shape).
        assert_eq!(v.get("system").and_then(|s| s.as_str()), Some("you are helpful"));
        // max_tokens preserved.
        assert_eq!(v.get("max_tokens").and_then(|m| m.as_u64()), Some(64));
        // messages array survives.
        assert!(v.get("messages").and_then(|m| m.as_array()).is_some());
    }

    // ---- Error classification --------------------------------------------

    #[test]
    fn throttling_exception_maps_to_rate_limited() {
        let err = classify_bedrock_error(
            reqwest::StatusCode::from_u16(400).unwrap(),
            br#"{"message":"Rate exceeded"}"#,
            Some("ThrottlingException"),
            Some(7),
        );
        match err {
            LlmError::RateLimited { retry_after_ms } => assert_eq!(retry_after_ms, 7_000),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn access_denied_maps_to_auth() {
        let err = classify_bedrock_error(
            reqwest::StatusCode::from_u16(400).unwrap(),
            br#"{"message":"You are not authorized"}"#,
            Some("AccessDeniedException"),
            None,
        );
        assert!(matches!(err, LlmError::Auth));
    }

    #[test]
    fn expired_token_maps_to_auth() {
        // Common when STS session creds expire mid-session.
        let err = classify_bedrock_error(
            reqwest::StatusCode::from_u16(403).unwrap(),
            br#"{"message":"The security token included in the request is expired"}"#,
            Some("ExpiredTokenException"),
            None,
        );
        assert!(matches!(err, LlmError::Auth));
    }

    #[test]
    fn validation_error_is_provider_with_message() {
        let err = classify_bedrock_error(
            reqwest::StatusCode::from_u16(400).unwrap(),
            br#"{"message":"max_tokens too high"}"#,
            Some("ValidationException"),
            None,
        );
        match err {
            LlmError::Provider { status, message } => {
                assert_eq!(status, 400);
                assert!(message.contains("max_tokens too high"));
            }
            other => panic!("expected Provider, got {other:?}"),
        }
    }

    #[test]
    fn http_429_without_amz_type_still_rate_limited() {
        let err = classify_bedrock_error(
            reqwest::StatusCode::from_u16(429).unwrap(),
            b"",
            None,
            None,
        );
        assert!(matches!(err, LlmError::RateLimited { retry_after_ms: 1000 }));
    }

    #[test]
    fn http_403_without_amz_type_still_auth() {
        let err = classify_bedrock_error(
            reqwest::StatusCode::from_u16(403).unwrap(),
            b"",
            None,
            None,
        );
        assert!(matches!(err, LlmError::Auth));
    }

    #[test]
    fn aws_error_message_extracted_from_capitalised_field() {
        // Some AWS services use Capitalised "Message" — we accept both.
        let m = extract_aws_error_message(r#"{"Message":"hi"}"#);
        assert_eq!(m.as_deref(), Some("hi"));
    }

    #[test]
    fn aws_error_message_extracted_from_lowercased_field() {
        let m = extract_aws_error_message(r#"{"message":"hi"}"#);
        assert_eq!(m.as_deref(), Some("hi"));
    }

    #[test]
    fn aws_error_message_returns_none_for_non_json_body() {
        let m = extract_aws_error_message("not json");
        assert!(m.is_none());
    }

    // ---- url_encode_path_segment helper ---------------------------------

    #[test]
    fn url_encode_keeps_unreserved_chars() {
        assert_eq!(
            url_encode_path_segment("AbZ-_.~09"),
            "AbZ-_.~09"
        );
    }

    #[test]
    fn url_encode_encodes_reserved_chars() {
        assert_eq!(url_encode_path_segment("a:b"), "a%3Ab");
        assert_eq!(url_encode_path_segment("a/b"), "a%2Fb");
        assert_eq!(url_encode_path_segment("a b"), "a%20b");
    }

    // ---- host_from_url helper -------------------------------------------

    #[test]
    fn host_from_url_https() {
        assert_eq!(
            host_from_url("https://api.example.com/path"),
            Some("api.example.com".to_string())
        );
    }

    #[test]
    fn host_from_url_http_with_port() {
        assert_eq!(
            host_from_url("http://localhost:8080/foo"),
            Some("localhost:8080".to_string())
        );
    }

    #[test]
    fn host_from_url_no_path() {
        assert_eq!(
            host_from_url("https://api.example.com"),
            Some("api.example.com".to_string())
        );
    }

    #[test]
    fn host_from_url_returns_none_for_empty() {
        // No real-world scheme → fallback caller handles it.
        let h = host_from_url("");
        assert!(h.is_none() || h.as_deref() == Some(""));
    }

    // ---- Provider trait ---------------------------------------------------

    #[test]
    fn provider_name_is_bedrock() {
        let p = BedrockProvider::from_agent_config("foo", &cfg());
        assert_eq!(p.name(), "bedrock");
    }

    #[test]
    fn supports_prompt_cache_true() {
        let p = BedrockProvider::from_agent_config("foo", &cfg());
        assert!(p.supports_prompt_cache());
    }

    #[test]
    fn supported_models_echoes_configured_model() {
        let p = BedrockProvider::from_agent_config("anthropic.claude-3-haiku-20240307-v1:0", &cfg());
        assert_eq!(
            p.supported_models(),
            vec!["anthropic.claude-3-haiku-20240307-v1:0".to_string()]
        );
    }

    // ---- chat() without credentials returns NotConfigured ----------------

    #[tokio::test]
    async fn chat_without_credentials_returns_not_configured() {
        let mut c = cfg();
        c.aws_access_key_env = Some("COS_BR_NOSUCH_AK_X1".into());
        c.aws_secret_key_env = Some("COS_BR_NOSUCH_SK_X1".into());
        let p = BedrockProvider::from_agent_config("foo", &c);
        let err = p.chat(req_text("hi")).await.unwrap_err();
        match err {
            LlmError::NotConfigured(msg) => {
                assert!(msg.contains("AWS"), "expected AWS in error msg: {msg}");
            }
            other => panic!("expected NotConfigured, got {other:?}"),
        }
    }

    // ---- Debug impl doesn't leak secrets --------------------------------

    #[test]
    fn debug_does_not_leak_secret_key() {
        let mut c = cfg();
        c.aws_access_key_env = Some("COS_BR_DBG_AK".into());
        c.aws_secret_key_env = Some("COS_BR_DBG_SK".into());
        std::env::set_var("COS_BR_DBG_AK", "AKID-REAL");
        std::env::set_var("COS_BR_DBG_SK", "SUPER-SECRET-DO-NOT-LEAK");
        let bc = BedrockConfig::from_agent_config("foo", &c);
        let s = format!("{:?}", bc);
        assert!(!s.contains("SUPER-SECRET"), "secret leaked in Debug: {s}");
        assert!(!s.contains("AKID-REAL"), "access key leaked in Debug: {s}");
        assert!(s.contains("credentials_present: true"));
        std::env::remove_var("COS_BR_DBG_AK");
        std::env::remove_var("COS_BR_DBG_SK");
    }

    // ---- Streaming URL & headers ----------------------------------------

    #[test]
    fn stream_model_path_uses_invoke_with_response_stream_suffix() {
        let mut c = cfg();
        c.aws_access_key_env = Some("COS_BR_STR_AK1".into());
        c.aws_secret_key_env = Some("COS_BR_STR_SK1".into());
        std::env::set_var("COS_BR_STR_AK1", "AKID");
        std::env::set_var("COS_BR_STR_SK1", "secret");
        let p = BedrockProvider::from_agent_config(
            "anthropic.claude-3-5-sonnet-20241022-v2:0",
            &c,
        );
        assert_eq!(
            p.stream_model_path(),
            "/model/anthropic.claude-3-5-sonnet-20241022-v2%3A0/invoke-with-response-stream"
        );
        std::env::remove_var("COS_BR_STR_AK1");
        std::env::remove_var("COS_BR_STR_SK1");
    }

    #[test]
    fn stream_model_path_encodes_arn_with_slashes_and_colons() {
        // Bedrock accepts full provisioned-model ARNs as model IDs.
        // Path-segment encoding must escape both `/` and `:`.
        let mut c = cfg();
        c.aws_access_key_env = Some("COS_BR_STR_AK2".into());
        c.aws_secret_key_env = Some("COS_BR_STR_SK2".into());
        std::env::set_var("COS_BR_STR_AK2", "AKID");
        std::env::set_var("COS_BR_STR_SK2", "secret");
        let arn = "arn:aws:bedrock:us-east-1:123:provisioned-model/abc";
        let p = BedrockProvider::from_agent_config(arn, &c);
        let path = p.stream_model_path();
        assert!(
            path.contains("arn%3Aaws%3Abedrock%3Aus-east-1%3A123%3Aprovisioned-model%2Fabc"),
            "ARN must be fully path-segment-encoded; got {path}"
        );
        assert!(path.ends_with("/invoke-with-response-stream"));
        std::env::remove_var("COS_BR_STR_AK2");
        std::env::remove_var("COS_BR_STR_SK2");
    }

    #[test]
    fn stream_full_url_combines_base_and_stream_path() {
        let mut c = cfg();
        c.aws_region = Some("eu-west-1".into());
        c.aws_access_key_env = Some("COS_BR_STR_AK3".into());
        c.aws_secret_key_env = Some("COS_BR_STR_SK3".into());
        std::env::set_var("COS_BR_STR_AK3", "AKID");
        std::env::set_var("COS_BR_STR_SK3", "secret");
        let p = BedrockProvider::from_agent_config("anthropic.claude-foo", &c);
        assert_eq!(
            p.stream_full_url(),
            "https://bedrock-runtime.eu-west-1.amazonaws.com/model/anthropic.claude-foo/invoke-with-response-stream"
        );
        std::env::remove_var("COS_BR_STR_AK3");
        std::env::remove_var("COS_BR_STR_SK3");
    }

    // ---- Streamed exception classifier ----------------------------------

    #[test]
    fn classify_throttling_lower_camel() {
        let e = stream_wire::classify_streamed_exception("throttlingException", "");
        assert!(matches!(e, LlmError::RateLimited { .. }), "got {e:?}");
    }

    #[test]
    fn classify_throttling_pascal_case_defensive_fallback() {
        // Some non-conforming clients emit PascalCase; our matcher
        // normalises the leading char so we still recognise it.
        let e = stream_wire::classify_streamed_exception("ThrottlingException", "");
        assert!(matches!(e, LlmError::RateLimited { .. }), "got {e:?}");
    }

    #[test]
    fn classify_validation_with_message_preserves_message() {
        let e = stream_wire::classify_streamed_exception(
            "validationException",
            "max tokens exceeded",
        );
        match e {
            LlmError::InvalidRequest(m) => {
                assert!(m.contains("max tokens exceeded"), "got {m}");
                assert!(m.contains("validationException"), "got {m}");
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[test]
    fn classify_model_stream_error_to_provider_500() {
        let e = stream_wire::classify_streamed_exception(
            "modelStreamErrorException",
            "decoder OOM",
        );
        assert!(
            matches!(e, LlmError::Provider { status: 500, ref message } if message.contains("decoder OOM"))
        );
    }

    #[test]
    fn classify_model_timeout_to_provider_504() {
        let e = stream_wire::classify_streamed_exception(
            "modelTimeoutException",
            "model failed to respond within 30s",
        );
        assert!(matches!(e, LlmError::Provider { status: 504, .. }));
    }

    #[test]
    fn classify_internal_server_to_provider_500() {
        let e = stream_wire::classify_streamed_exception("internalServerException", "");
        assert!(matches!(e, LlmError::Provider { status: 500, .. }));
    }

    #[test]
    fn classify_service_unavailable_to_provider_503() {
        let e = stream_wire::classify_streamed_exception("serviceUnavailableException", "");
        assert!(matches!(e, LlmError::Provider { status: 503, .. }));
    }

    #[test]
    fn classify_unknown_exception_surfaces_as_provider_500_and_includes_name() {
        // Unknown exception names must NOT be silently swallowed —
        // surface them so observability picks up new AWS-side error
        // taxonomy expansions.
        let e = stream_wire::classify_streamed_exception(
            "newSurpriseException",
            "explanatory text",
        );
        match e {
            LlmError::Provider { status, message } => {
                assert_eq!(status, 500);
                assert!(message.contains("newSurpriseException"), "got {message}");
                assert!(message.contains("explanatory text"), "got {message}");
            }
            other => panic!("expected Provider, got {other:?}"),
        }
    }

    // ---- BedrockStream end-to-end (synthetic frames) --------------------

    use crate::agent::llm::aws_eventstream::encode_frame;
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use bytes::Bytes;
    use futures_util::stream as futstream;
    use futures_util::StreamExt;

    /// Build one event-frame whose inner SSE data field is `inner_json`.
    fn event_frame_json(inner_json: &str) -> Vec<u8> {
        let outer = serde_json::json!({
            "bytes": B64.encode(inner_json),
        });
        encode_frame(
            &[(":message-type", "event"), (":event-type", "chunk")],
            outer.to_string().as_bytes(),
        )
    }

    fn anthropic_event_json(kind: &str, extra: serde_json::Value) -> String {
        let mut obj = match extra {
            serde_json::Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        obj.insert("type".into(), serde_json::Value::String(kind.into()));
        serde_json::Value::Object(obj).to_string()
    }

    fn collect(
        body: Vec<Vec<u8>>,
    ) -> Vec<crate::agent::llm::Result<crate::agent::llm::StreamEvent>> {
        let chunks: Vec<std::result::Result<Bytes, reqwest::Error>> =
            body.into_iter().map(|v| Ok(Bytes::from(v))).collect();
        let s = futstream::iter(chunks);
        let stream = stream_wire::BedrockStream::new(s, "claude-3-5-sonnet-20241022");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async { stream.collect::<Vec<_>>().await })
    }

    #[test]
    fn stream_handles_full_text_lifecycle_with_message_stop() {
        let frames = vec![
            event_frame_json(&anthropic_event_json(
                "message_start",
                serde_json::json!({
                    "message": {
                        "id": "msg_1",
                        "model": "claude-3-5-sonnet-20241022",
                        "usage": { "input_tokens": 12, "output_tokens": 0 }
                    }
                }),
            )),
            event_frame_json(&anthropic_event_json(
                "content_block_start",
                serde_json::json!({
                    "index": 0,
                    "content_block": { "type": "text", "text": "" }
                }),
            )),
            event_frame_json(&anthropic_event_json(
                "content_block_delta",
                serde_json::json!({
                    "index": 0,
                    "delta": { "type": "text_delta", "text": "Hello" }
                }),
            )),
            event_frame_json(&anthropic_event_json(
                "content_block_delta",
                serde_json::json!({
                    "index": 0,
                    "delta": { "type": "text_delta", "text": " world" }
                }),
            )),
            event_frame_json(&anthropic_event_json(
                "content_block_stop",
                serde_json::json!({ "index": 0 }),
            )),
            event_frame_json(&anthropic_event_json(
                "message_delta",
                serde_json::json!({
                    "delta": { "stop_reason": "end_turn" },
                    "usage": { "output_tokens": 7 }
                }),
            )),
            event_frame_json(&anthropic_event_json("message_stop", serde_json::json!({}))),
        ];
        let events = collect(frames);
        let oks: Vec<_> = events.iter().map(|r| r.as_ref().unwrap()).collect();
        // Expect text deltas + final Done (no Message in streaming).
        let text: String = oks
            .iter()
            .filter_map(|e| match e {
                crate::agent::llm::StreamEvent::TextDelta { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello world");
        let done = oks.iter().rev().find(|e| {
            matches!(e, crate::agent::llm::StreamEvent::Done { .. })
        });
        assert!(done.is_some(), "expected Done event; got {oks:?}");
    }

    #[test]
    fn stream_emits_tool_input_delta_then_final_tool_use() {
        let frames = vec![
            event_frame_json(&anthropic_event_json(
                "message_start",
                serde_json::json!({
                    "message": {
                        "id": "msg_2",
                        "model": "claude-3-5-sonnet-20241022",
                        "usage": { "input_tokens": 5, "output_tokens": 0 }
                    }
                }),
            )),
            event_frame_json(&anthropic_event_json(
                "content_block_start",
                serde_json::json!({
                    "index": 0,
                    "content_block": {
                        "type": "tool_use",
                        "id": "toolu_xyz",
                        "name": "echo",
                        "input": {}
                    }
                }),
            )),
            event_frame_json(&anthropic_event_json(
                "content_block_delta",
                serde_json::json!({
                    "index": 0,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": "{\"text\":\"hi"
                    }
                }),
            )),
            event_frame_json(&anthropic_event_json(
                "content_block_delta",
                serde_json::json!({
                    "index": 0,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": "\"}"
                    }
                }),
            )),
            event_frame_json(&anthropic_event_json(
                "content_block_stop",
                serde_json::json!({ "index": 0 }),
            )),
            event_frame_json(&anthropic_event_json(
                "message_delta",
                serde_json::json!({
                    "delta": { "stop_reason": "tool_use" },
                    "usage": { "output_tokens": 9 }
                }),
            )),
            event_frame_json(&anthropic_event_json("message_stop", serde_json::json!({}))),
        ];
        let events = collect(frames);
        let oks: Vec<_> = events.iter().map(|r| r.as_ref().unwrap()).collect();
        // Expect at least one ToolUseStart, ToolInputDelta(s), ToolUse final.
        assert!(
            oks.iter().any(|e| matches!(e, crate::agent::llm::StreamEvent::ToolUseStart { .. })),
            "missing ToolUseStart in {oks:?}"
        );
        let final_tool_use = oks.iter().find_map(|e| match e {
            crate::agent::llm::StreamEvent::ToolUse(tc) => Some(tc),
            _ => None,
        });
        let tc = final_tool_use.expect("missing final ToolUse event");
        assert_eq!(tc.name, "echo");
        assert_eq!(tc.id, "toolu_xyz");
        assert_eq!(tc.input, serde_json::json!({"text": "hi"}));
    }

    #[test]
    fn stream_exception_frame_maps_to_rate_limited() {
        let frames = vec![encode_frame(
            &[
                (":message-type", "exception"),
                (":exception-type", "throttlingException"),
                (":content-type", "application/json"),
            ],
            br#"{"message":"rate exceeded"}"#,
        )];
        let events = collect(frames);
        // Last (only) event must be a RateLimited error.
        assert_eq!(events.len(), 1);
        let err = events[0].as_ref().unwrap_err();
        assert!(
            matches!(err, LlmError::RateLimited { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn stream_exception_frame_unknown_name_surfaces_provider_error() {
        let frames = vec![encode_frame(
            &[
                (":message-type", "exception"),
                (":exception-type", "newCloudExpansionException"),
                (":content-type", "application/json"),
            ],
            br#"{"message":"future taxonomy"}"#,
        )];
        let events = collect(frames);
        assert_eq!(events.len(), 1);
        let err = events[0].as_ref().unwrap_err();
        match err {
            LlmError::Provider { status, message } => {
                assert_eq!(*status, 500);
                assert!(
                    message.contains("newCloudExpansionException"),
                    "got {message}"
                );
            }
            other => panic!("expected Provider, got {other:?}"),
        }
    }

    #[test]
    fn stream_unmodeled_error_frame_maps_to_provider() {
        let frames = vec![encode_frame(
            &[
                (":message-type", "error"),
                (":error-code", "InternalServerError"),
                (":error-message", "an error has occurred"),
            ],
            b"",
        )];
        let events = collect(frames);
        assert_eq!(events.len(), 1);
        let err = events[0].as_ref().unwrap_err();
        match err {
            LlmError::Provider { status, message } => {
                assert_eq!(*status, 500);
                assert!(message.contains("InternalServerError"), "got {message}");
                assert!(message.contains("an error has occurred"), "got {message}");
            }
            other => panic!("expected Provider, got {other:?}"),
        }
    }

    #[test]
    fn stream_unknown_message_type_is_silently_ignored_for_forward_compat() {
        // An unrecognised :message-type that arrives BEFORE the
        // real terminator (message_stop) must not poison the stream.
        let frames = vec![
            encode_frame(
                &[(":message-type", "futureKindWeDontKnow")],
                b"opaque payload",
            ),
            event_frame_json(&anthropic_event_json(
                "message_start",
                serde_json::json!({
                    "message": {
                        "id": "msg_3",
                        "model": "claude-3-5-sonnet-20241022",
                        "usage": { "input_tokens": 1, "output_tokens": 0 }
                    }
                }),
            )),
            event_frame_json(&anthropic_event_json(
                "message_delta",
                serde_json::json!({
                    "delta": { "stop_reason": "end_turn" },
                    "usage": { "output_tokens": 1 }
                }),
            )),
            event_frame_json(&anthropic_event_json("message_stop", serde_json::json!({}))),
        ];
        let events = collect(frames);
        let oks: Vec<_> = events.iter().map(|r| r.as_ref().unwrap()).collect();
        assert!(
            oks.iter().any(|e| matches!(e, crate::agent::llm::StreamEvent::Done { .. })),
            "stream should still complete normally; got {oks:?}"
        );
    }

    #[test]
    fn stream_truncated_body_at_eof_surfaces_stream_error() {
        // Build a complete frame, then chop off the final 4 bytes so
        // the parser sees a truncated tail at EOF.
        let mut frame = event_frame_json(&anthropic_event_json(
            "message_start",
            serde_json::json!({
                "message": {
                    "id": "msg_4",
                    "model": "claude-3-5-sonnet-20241022",
                    "usage": { "input_tokens": 1, "output_tokens": 0 }
                }
            }),
        ));
        let n = frame.len();
        frame.truncate(n - 4);
        let events = collect(vec![frame]);
        // Should contain at least one Stream error at the end.
        let last = events.last().expect("at least one event");
        let err = last.as_ref().unwrap_err();
        assert!(
            matches!(err, LlmError::Stream(_)),
            "expected Stream error; got {err:?}"
        );
    }

    #[test]
    fn stream_bad_message_crc_surfaces_stream_error_and_terminates() {
        let mut frame = event_frame_json(&anthropic_event_json(
            "message_start",
            serde_json::json!({
                "message": {
                    "id": "msg_5",
                    "model": "claude-3-5-sonnet-20241022",
                    "usage": { "input_tokens": 1, "output_tokens": 0 }
                }
            }),
        ));
        // Corrupt the message CRC (last 4 bytes are the trailer).
        let n = frame.len();
        frame[n - 1] ^= 0xff;
        // Even if more frames follow, the stream must terminate at
        // the corrupt frame with an error.
        let events = collect(vec![frame, event_frame_json("{}")]);
        let any_err = events
            .iter()
            .any(|r| matches!(r, Err(LlmError::Stream(_))));
        assert!(any_err, "expected at least one Stream error; got {events:?}");
    }
}
