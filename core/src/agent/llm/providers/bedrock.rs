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
//! ## What we deliberately don't do (yet)
//!
//! - Streaming via `/invoke-with-response-stream` (uses
//!   AWS-EventStream framing — non-trivial; we ship the same
//!   non-SSE shim as Anthropic for now).
//! - IMDS / SSO token fetching — env vars only.
//! - Cross-region failover.
//! - Bedrock-Agent / Knowledge-Base APIs (those are different
//!   endpoints; this provider is for `bedrock-runtime` only).

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
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
        // Same non-SSE shim as Anthropic provider. Real
        // /invoke-with-response-stream support requires
        // AWS-EventStream framing — deferred (not blocking the
        // critical agent loop, which polls per-turn).
        let response = self.chat(request).await?;
        let finish = response.finish_reason;
        let usage = response.usage.clone();
        let events: Vec<std::result::Result<StreamEvent, LlmError>> = vec![
            Ok(StreamEvent::Message(response)),
            Ok(StreamEvent::Done { finish, usage }),
        ];
        Ok(stream::iter(events).boxed())
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
}
