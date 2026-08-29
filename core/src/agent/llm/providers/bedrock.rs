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
use crate::agent::llm::construction::HttpTransport;
use crate::agent::llm::construction::ProcessCredentialSource;
use crate::agent::llm::sigv4::{
    current_amz_date, sign, AwsCredentials, SignableRequest, SigningContext,
};
use crate::agent::llm::{ChatRequest, ChatResponse, LlmError, Provider, Result, StreamEvent};
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
            .field("credentials_present", &self.credentials.as_ref().is_some())
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
        Self::try_from_agent_config(model, agent).unwrap_or_else(|error| {
            tracing::error!(
                error = %error,
                "legacy Bedrock constructor deferred provider infrastructure failure"
            );
            Self::unconfigured(model, agent)
        })
    }

    pub fn try_from_agent_config(model: &str, agent: &AgentConfig) -> Result<Self> {
        crate::agent::llm::registry::bedrock_config(model, agent, &ProcessCredentialSource)
    }

    fn unconfigured(model: &str, agent: &AgentConfig) -> Self {
        Self {
            region: agent
                .aws_region
                .clone()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_REGION.to_string()),
            base_url: agent.base_url.clone().filter(|value| !value.is_empty()),
            model: model.to_string(),
            credentials: None,
            extra_headers: agent.extra_headers.clone(),
            request_timeout: Duration::from_secs(agent.request_timeout),
        }
    }

    /// Region-derived host for SigV4 signing AND the URL we POST to.
    /// Returns the host portion only (no scheme), because the SigV4
    /// canonical headers include `host:` without scheme/port.
    fn host(&self) -> String {
        if let Some(url) = &self.base_url {
            // Strip scheme + path so the canonical host header matches
            // what reqwest writes on the wire.
            host_from_url(url)
                .unwrap_or_else(|| format!("bedrock-runtime.{}.amazonaws.com", self.region))
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

pub struct BedrockProvider {
    cfg: BedrockConfig,
    transport: HttpTransport,
    initialization_error: Option<Arc<crate::agent::llm::ProviderInitializationError>>,
}

impl BedrockProvider {
    pub fn new(cfg: BedrockConfig) -> Self {
        Self::new_with_transport(cfg, HttpTransport::legacy_default())
    }

    pub fn new_with_transport(cfg: BedrockConfig, transport: HttpTransport) -> Self {
        Self {
            cfg,
            transport,
            initialization_error: None,
        }
    }

    pub fn from_agent_config(model: &str, agent: &AgentConfig) -> Self {
        match BedrockConfig::try_from_agent_config(model, agent) {
            Ok(config) => Self::new(config),
            Err(error) => {
                tracing::error!(error = %error, "legacy Bedrock provider initialization failed");
                Self {
                    cfg: BedrockConfig::unconfigured(model, agent),
                    transport: HttpTransport::legacy_default(),
                    initialization_error: Some(Arc::new(
                        crate::agent::llm::ProviderInitializationError::new(PROVIDER_NAME, error),
                    )),
                }
            }
        }
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

    fn ensure_initialized(&self) -> Result<()> {
        match &self.initialization_error {
            Some(error) => Err(crate::agent::llm::deferred_initialization_error(error)),
            None => Ok(()),
        }
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
        self.initialization_error.is_none()
            && self.transport.is_ready()
            && self.cfg.credentials.is_some()
    }

    fn supports_prompt_cache(&self) -> bool {
        // Bedrock-side Anthropic models accept the same
        // `cache_control: ephemeral` markers — propagate the
        // capability so prompt caching turns on for cached prompts.
        true
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.ensure_initialized()?;
        let creds = self.cfg.credentials.as_ref().ok_or_else(|| {
            LlmError::NotConfigured(
                "bedrock: missing AWS credentials (set AWS_ACCESS_KEY_ID + \
                 AWS_SECRET_ACCESS_KEY env vars or aws_*_credential / aws_*_env \
                 fields in [agent])"
                    .into(),
            )
        })?;

        let body_bytes = build_bedrock_body_bytes(&request)?;

        let amz_date =
            current_amz_date().map_err(|e| LlmError::InvalidRequest(format!("clock: {e}")))?;
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
            headers: &[("content-type".to_string(), "application/json".to_string())],
            body: &body_bytes,
        };
        let signed = sign(creds, &ctx, &host, &signable);

        let mut http = self
            .transport
            .post(self.full_url(), self.cfg.request_timeout)?
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
        let bytes =
            crate::agent::llm::read_body_capped(resp, crate::agent::llm::MAX_NONSTREAM_BODY_BYTES)
                .await?;

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
            .map_err(|e| LlmError::UpstreamMalformed(format!("bedrock response: {e}")))?;
        anthropic_wire::response_to_chat(parsed, &self.cfg.model)
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        self.ensure_initialized()?;
        let creds = self.cfg.credentials.as_ref().ok_or_else(|| {
            LlmError::NotConfigured(
                "bedrock: missing AWS credentials (set AWS_ACCESS_KEY_ID + \
                 AWS_SECRET_ACCESS_KEY env vars or aws_*_credential / aws_*_env \
                 fields in [agent])"
                    .into(),
            )
        })?;

        let body_bytes = build_bedrock_body_bytes(&request)?;

        let amz_date =
            current_amz_date().map_err(|e| LlmError::InvalidRequest(format!("clock: {e}")))?;
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
            headers: &[("content-type".to_string(), "application/json".to_string())],
            body: &body_bytes,
        };
        let signed = sign(creds, &ctx, &host, &signable);

        let mut http = self
            .transport
            .post(self.stream_full_url(), self.cfg.request_timeout)?
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
            let bytes = crate::agent::llm::read_body_capped(
                resp,
                crate::agent::llm::MAX_NONSTREAM_BODY_BYTES,
            )
            .await
            .unwrap_or_default();
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
        .map(|m| crate::agent::llm::redact_body_for_error(&m))
        .unwrap_or_else(|| crate::agent::llm::redact_body_for_error(&body_text));

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
    use crate::agent::llm::aws_eventstream::{EventStreamParser, Frame, FrameError};
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
        S: Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + 'static,
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
            let outer: serde_json::Value = match serde_json::from_slice(&frame.payload) {
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
        S: Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
    {
        type Item = Result<StreamEvent>;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
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
                    match p.finish() {
                        Err(FrameError::Truncated(n)) => {
                            self.pending.push_back(Err(LlmError::Stream(format!(
                                "bedrock event stream truncated: {n} \
                                     byte(s) of partial frame at EOF"
                            ))));
                        }
                        Err(other) => {
                            self.pending.push_back(Err(LlmError::Stream(format!(
                                "bedrock event stream framing: {other}"
                            ))));
                        }
                        Ok(()) if !self.converter.is_finished() => {
                            self.pending.push_back(Err(LlmError::UpstreamMalformed(
                                "bedrock stream ended before message_stop".into(),
                            )));
                        }
                        Ok(()) => {}
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
                        self.pending.push_back(Err(LlmError::Transport(e)));
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
            "throttlingException" => LlmError::RateLimited { retry_after_ms: 0 },
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/providers/bedrock.rs"
    ));
}
