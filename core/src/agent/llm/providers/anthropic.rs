//! Anthropic Messages API provider.
//!
//! Anthropic's wire format is similar in spirit to OpenAI's chat-completions
//! but differs in shape:
//!
//! - Endpoint: `POST /v1/messages` (default base `https://api.anthropic.com`)
//! - Auth header is `x-api-key: <key>`, not bearer
//! - Required `anthropic-version: 2023-06-01` header
//! - `system` is a top-level string, NOT a message in `messages[]`
//! - `max_tokens` is **required** (we default to 4096 if the caller omits)
//! - Tool result is a content block inside a `user` message, not a separate
//!   `tool` role
//! - Tools are flat objects (`{name, description, input_schema}`), no
//!   nested `function` wrapper
//! - `stop_reason` enum: `"end_turn" | "tool_use" | "max_tokens" | "stop_sequence"`
//!
//! Configuration model (read at construction by `registry::build`):
//!
//!   - `base_url`              `https://api.anthropic.com` (provider default)
//!   - `api_key_credential`    name of the cred in `cos credential` namespace `agent`
//!   - `api_key_env`           env var fallback (e.g. `ANTHROPIC_API_KEY`)
//!   - `extra_headers`         arbitrary extra headers
//!   - `request_timeout`       per-request timeout, seconds
//!
//! Tool-calling: tools are forwarded as flat `{name, description, input_schema}`
//! entries; the response's `tool_use` content blocks are mapped to
//! [`ContentBlock::ToolUse`] and the parallel [`ToolCall`] vector. Multi-turn
//! tool flows work end-to-end.
//!
//! Streaming: ships the same non-SSE shim as `openai_compat` — calls `chat()`
//! then emits `Message + Done`. Real SSE streaming lands when a use case
//! demands it (Phase 5 alongside prompt caching).

use async_trait::async_trait;
use futures_util::stream::{BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::openai_compat::resolve_api_key;
use crate::agent::llm::{
    ChatRequest, ChatResponse, ContentBlock, FinishReason, LlmError, Provider, Result, Role,
    StreamEvent, Tool, ToolCall, ToolChoice, Usage,
};
use crate::agent::prompt::caching;
use crate::config::AgentConfig;

pub const PROVIDER_NAME: &str = "anthropic";

const DEFAULT_BASE: &str = "https://api.anthropic.com";

/// Anthropic API version pin. Bump when consciously upgrading the wire
/// format (the API is versioned and stable, so this changes rarely).
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Fallback `max_tokens` when the caller didn't set one. Anthropic
/// requires this field — sending no value or 0 yields a 400.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Resolve the default base URL. Trivial today (one upstream), kept as a
/// helper so future Anthropic-on-Bedrock or AWS-vended-endpoint configs
/// can branch here.
pub fn default_base_url() -> &'static str {
    DEFAULT_BASE
}

#[derive(Clone)]
pub struct AnthropicConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub extra_headers: HashMap<String, String>,
    pub request_timeout: Duration,
    /// Optional multi-key credential pool. See
    /// `OpenAICompatConfig::pool` for semantics.
    pub pool: Option<std::sync::Arc<crate::agent::llm::credential_pool::Pool>>,
}

impl std::fmt::Debug for AnthropicConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicConfig")
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

impl AnthropicConfig {
    pub fn from_agent_config(model: &str, agent: &AgentConfig) -> Self {
        let base_url = agent
            .base_url
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE.to_string());

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

        let pool = match crate::agent::llm::credential_pool::Pool::try_from_agent_config(
            "provider:anthropic",
            agent,
        ) {
            Ok(Some(p)) => Some(std::sync::Arc::new(p)),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(
                    target: "cos::agent::llm::pool",
                    "credential pool for provider 'anthropic' declared but unresolved: {e}; \
                     falling back to single-key path"
                );
                None
            }
        };

        Self {
            base_url,
            api_key,
            model: model.to_string(),
            extra_headers: agent.extra_headers.clone(),
            request_timeout,
            pool,
        }
    }
}

pub struct AnthropicProvider {
    cfg: AnthropicConfig,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(cfg: AnthropicConfig) -> Self {
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("cos-agent/", env!("CARGO_PKG_VERSION")))
            // MEDIUM-14: per-phase HTTP timeout. `connect_timeout`
            // bounds TCP + TLS independently of the overall request
            // budget so a black-holed host doesn't tie up a worker.
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(60));
        if cfg.request_timeout > Duration::from_secs(0) {
            builder = builder.timeout(cfg.request_timeout);
        }
        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        Self { cfg, client }
    }

    pub fn from_agent_config(model: &str, agent: &AgentConfig) -> Self {
        Self::new(AnthropicConfig::from_agent_config(model, agent))
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.cfg.base_url)
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn supported_models(&self) -> Vec<String> {
        vec![self.cfg.model.clone()]
    }

    fn is_configured(&self) -> bool {
        self.cfg.api_key.is_some() || self.cfg.pool.as_ref().is_some_and(|p| !p.is_empty())
    }

    fn supports_prompt_cache(&self) -> bool {
        true
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = wire::build_request_body(&request, &self.cfg.model, false);

        let lease = if let Some(pool) = &self.cfg.pool {
            match pool.acquire() {
                Ok(l) => Some(l),
                Err(e) => return Err(LlmError::NotConfigured(format!("pool: {e}"))),
            }
        } else {
            None
        };
        let api_key: Option<&str> = match &lease {
            Some(l) => Some(l.value()),
            None => self.cfg.api_key.as_deref(),
        };

        let mut http = self
            .client
            .post(self.endpoint())
            .header("Content-Type", "application/json")
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body);

        if let Some(key) = api_key {
            http = http.header("x-api-key", key);
        }
        for (k, v) in &self.cfg.extra_headers {
            http = http.header(k.as_str(), v.as_str());
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
        let retry_after_secs = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        // HIGH-5: cap the response body.
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
            let err = wire::classify_http_error(status, &bytes, retry_after_secs);
            if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                let body_str = std::str::from_utf8(&bytes).unwrap_or("");
                let cls = crate::agent::llm::error_classifier::classify(status.as_u16(), body_str);
                pool.report_failure(l, cls);
            }
            return Err(err);
        }

        let parsed: wire::Response = match serde_json::from_slice(&bytes) {
            Ok(p) => p,
            Err(e) => {
                if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                    pool.report_failure(
                        l,
                        crate::agent::llm::credential_pool::FailureClass::CallerError,
                    );
                }
                return Err(LlmError::Parse(e.to_string()));
            }
        };

        let result = wire::response_to_chat(parsed, &self.cfg.model);
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
        // Real SSE streaming. Build the request body with stream:true,
        // hit /v1/messages with Accept: text/event-stream, validate
        // the HTTP status synchronously (so 401/429/etc surface
        // immediately), then wrap the bytes stream in our SSE
        // parser + Anthropic event converter.
        let body = wire::build_request_body(&request, &self.cfg.model, true);

        let lease = if let Some(pool) = &self.cfg.pool {
            match pool.acquire() {
                Ok(l) => Some(l),
                Err(e) => return Err(LlmError::NotConfigured(format!("pool: {e}"))),
            }
        } else {
            None
        };
        let api_key: Option<&str> = match &lease {
            Some(l) => Some(l.value()),
            None => self.cfg.api_key.as_deref(),
        };

        let mut http = self
            .client
            .post(self.endpoint())
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body);

        if let Some(key) = api_key {
            http = http.header("x-api-key", key);
        }
        for (k, v) in &self.cfg.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }

        let resp = http.send().await.map_err(|e| {
            if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                pool.report_failure(
                    l,
                    crate::agent::llm::error_classifier::classify_network_error(),
                );
            }
            LlmError::Transport(e)
        })?;

        let status = resp.status();
        let retry_after_secs = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        if !status.is_success() {
            let bytes = crate::agent::llm::read_body_capped(
                resp,
                crate::agent::llm::MAX_NONSTREAM_BODY_BYTES,
            )
            .await
            .unwrap_or_default();
            let err = wire::classify_http_error(status, &bytes, retry_after_secs);
            if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                let body_str = std::str::from_utf8(&bytes).unwrap_or("");
                let cls = crate::agent::llm::error_classifier::classify(status.as_u16(), body_str);
                pool.report_failure(l, cls);
            }
            return Err(err);
        }

        // MEDIUM-9: previously we credited the lease as soon as the
        // upstream returned 200 headers, which masked stalls and
        // mid-stream failures from the credential pool. We now move
        // that accounting into `AnthropicStream` so it fires only
        // when the body actually completes (or, on mid-stream
        // failure, charges the lease).
        let bytes_stream = resp.bytes_stream();
        let model = self.cfg.model.clone();
        let stream = wire::AnthropicStream::new(
            bytes_stream,
            &model,
            self.cfg.pool.clone(),
            lease,
        );
        Ok(stream.boxed())
    }
}

/// Wire-format adapters: serialise our internal types into Anthropic's
/// Messages schema and parse responses back. Kept private + pure (no IO)
/// so we can unit-test it without spinning up an HTTP server.
pub(crate) mod wire {
    use super::*;
    use crate::agent::llm::sse::SseEvent;

    // --- Request --------------------------------------------------------

    /// Build the JSON body for `POST /v1/messages`. Pure — no IO.
    ///
    /// Key differences vs OpenAI:
    /// - `system` is hoisted out of `messages` to top level
    /// - `max_tokens` is always present (Anthropic requires it)
    /// - tool result blocks live inside a `user` message, not their own role
    ///
    /// Honours prompt-cache markers from
    /// [`crate::agent::prompt::caching`]: `cache_control: {"type":"ephemeral"}`
    /// is attached to the last content block of any breakpoint message,
    /// to the system block when `cache_system` is set, and to the last
    /// tool when `cache_tools` is set. Markers are consumed (stripped
    /// from a working copy of `extra`) so they never appear on the wire.
    pub(crate) fn build_request_body(
        request: &ChatRequest,
        model: &str,
        stream: bool,
    ) -> serde_json::Value {
        // Work on a clone so we can consume cache markers without
        // mutating the caller's request.
        let mut working = request.clone();
        let markers = caching::consume_markers(&mut working);
        let bp_set: std::collections::HashSet<u32> = markers.breakpoints.iter().copied().collect();
        let last_msg_idx = working.messages.len().saturating_sub(1);

        let messages: Vec<serde_json::Value> = working
            .messages
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let cache_this = bp_set.contains(&(i as u32)) && i <= last_msg_idx;
                message_to_json(m, cache_this)
            })
            .collect();

        let tools_count = working.tools.len();
        let tools: Vec<serde_json::Value> = working
            .tools
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let cache_this = markers.cache_tools && i + 1 == tools_count;
                tool_to_json(t, cache_this)
            })
            .collect();

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "max_tokens": working.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        });

        if let Some(obj) = body.as_object_mut() {
            if let Some(sys) = &working.system {
                if !sys.is_empty() {
                    if markers.cache_system {
                        // Promote string → content-block array so we
                        // have a place to hang cache_control.
                        obj.insert(
                            "system".into(),
                            serde_json::json!([{
                                "type": "text",
                                "text": sys,
                                "cache_control": {"type": "ephemeral"},
                            }]),
                        );
                    } else {
                        obj.insert("system".into(), serde_json::json!(sys));
                    }
                }
            }
            if !tools.is_empty() {
                obj.insert("tools".into(), serde_json::Value::Array(tools));
                obj.insert(
                    "tool_choice".into(),
                    tool_choice_to_json(&working.tool_choice),
                );
            }
            if let Some(v) = working.temperature {
                obj.insert("temperature".into(), serde_json::json!(v));
            }
            if let Some(v) = working.top_p {
                obj.insert("top_p".into(), serde_json::json!(v));
            }
            if !working.stop_sequences.is_empty() {
                obj.insert(
                    "stop_sequences".into(),
                    serde_json::json!(working.stop_sequences),
                );
            }
            if stream {
                obj.insert("stream".into(), serde_json::json!(true));
            }
            for (key, value) in working.provider_extra_fields() {
                obj.insert(key.to_owned(), value.clone());
            }
        }
        body
    }

    /// Anthropic accepts only `user` and `assistant` roles. System prompts
    /// are hoisted out at the top level (handled by [`build_request_body`]).
    /// Tool-result content lives inside a `user` message.
    fn role_to_str(role: Role) -> &'static str {
        match role {
            Role::Assistant => "assistant",
            // System should have been hoisted; treat as user defensively.
            Role::System | Role::User | Role::Tool => "user",
        }
    }

    fn message_to_json(
        m: &crate::agent::llm::Message,
        cache_breakpoint: bool,
    ) -> serde_json::Value {
        let role = role_to_str(m.role);

        // System messages would have been hoisted out before getting here.
        // If one slipped through, fall through to the generic content path
        // — Anthropic will surface the bug as a 400 (no system role).
        if matches!(m.role, Role::System) {
            return serde_json::json!({
                "role": "user",
                "content": text_only_content(&m.content),
            });
        }

        let mut blocks: Vec<serde_json::Value> =
            m.content.iter().filter_map(content_block_to_json).collect();

        // Attach cache_control to the LAST content block of this
        // message when this message is a breakpoint. No-op if the
        // message has no content blocks.
        if cache_breakpoint {
            if let Some(last) = blocks.last_mut() {
                if let Some(obj) = last.as_object_mut() {
                    obj.insert(
                        "cache_control".into(),
                        serde_json::json!({"type": "ephemeral"}),
                    );
                }
            }
        }

        serde_json::json!({
            "role": role,
            "content": blocks,
        })
    }

    fn text_only_content(blocks: &[ContentBlock]) -> String {
        let mut out = String::new();
        for b in blocks {
            if let ContentBlock::Text { text } = b {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }

    fn content_block_to_json(b: &ContentBlock) -> Option<serde_json::Value> {
        match b {
            ContentBlock::Text { text } => Some(serde_json::json!({
                "type": "text",
                "text": text,
            })),
            ContentBlock::ToolUse { id, name, input } => Some(serde_json::json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            })),
            ContentBlock::ToolResult {
                tool_use_id,
                is_error,
                content,
            } => {
                let mut obj = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content,
                });
                if *is_error {
                    if let Some(o) = obj.as_object_mut() {
                        o.insert("is_error".into(), serde_json::json!(true));
                    }
                }
                Some(obj)
            }
            ContentBlock::Reasoning { summary, .. } => (!summary.is_empty()).then(|| {
                serde_json::json!({
                    "type": "text",
                    "text": summary.join("\n"),
                })
            }),
            ContentBlock::ToolState { .. } => None,
            ContentBlock::Image { media_type, data } => Some(serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type,
                    "data": data,
                },
            })),
        }
    }

    fn tool_to_json(t: &Tool, cache_breakpoint: bool) -> serde_json::Value {
        let mut obj = serde_json::json!({
            "name": t.name,
            "description": t.description,
            "input_schema": t.input_schema,
        });
        if cache_breakpoint {
            if let Some(o) = obj.as_object_mut() {
                o.insert(
                    "cache_control".into(),
                    serde_json::json!({"type": "ephemeral"}),
                );
            }
        }
        obj
    }

    fn tool_choice_to_json(c: &ToolChoice) -> serde_json::Value {
        match c {
            ToolChoice::Auto => serde_json::json!({"type": "auto"}),
            // Anthropic doesn't have a "none" — `auto` is the closest.
            // The tools array being empty is the real "no tools" signal,
            // but if a caller passes ToolChoice::None alongside tools,
            // hint via auto + the upstream may still ignore tools.
            ToolChoice::None => serde_json::json!({"type": "auto"}),
            ToolChoice::Required => serde_json::json!({"type": "any"}),
            ToolChoice::Tool { name } => serde_json::json!({
                "type": "tool",
                "name": name,
            }),
        }
    }

    // --- Response -------------------------------------------------------

    #[derive(Debug, Deserialize)]
    pub(crate) struct Response {
        #[serde(default)]
        pub model: Option<String>,
        #[serde(default)]
        pub content: Vec<ContentBlockJson>,
        #[serde(default)]
        pub stop_reason: Option<String>,
        #[serde(default)]
        pub usage: Option<UsageJson>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub(crate) enum ContentBlockJson {
        Text {
            text: String,
        },
        ToolUse {
            id: String,
            name: String,
            #[serde(default)]
            input: serde_json::Value,
        },
        // Forward-compat for blocks we don't model yet (thinking, image, etc.)
        #[serde(other)]
        Other,
    }

    #[derive(Debug, Default, Deserialize, Serialize)]
    pub(crate) struct UsageJson {
        #[serde(default)]
        pub input_tokens: u32,
        #[serde(default)]
        pub output_tokens: u32,
        #[serde(default)]
        pub cache_read_input_tokens: u32,
        #[serde(default)]
        pub cache_creation_input_tokens: u32,
    }

    pub(crate) fn response_to_chat(resp: Response, fallback_model: &str) -> Result<ChatResponse> {
        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        for block in resp.content {
            match block {
                ContentBlockJson::Text { text } => {
                    content_blocks.push(ContentBlock::Text { text });
                }
                ContentBlockJson::ToolUse { id, name, input } => {
                    content_blocks.push(ContentBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    });
                    tool_calls.push(ToolCall { id, name, input });
                }
                ContentBlockJson::Other => {}
            }
        }

        let finish_reason = match resp.stop_reason.as_deref() {
            Some("end_turn") | Some("stop_sequence") | None => FinishReason::Stop,
            Some("max_tokens") => FinishReason::Length,
            Some("tool_use") => FinishReason::ToolUse,
            Some("refusal") => FinishReason::Refusal,
            Some(_) => FinishReason::Other,
        };

        let usage = resp
            .usage
            .map(|u| Usage {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                cache_read_tokens: u.cache_read_input_tokens,
                cache_write_tokens: u.cache_creation_input_tokens,
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
    /// `retry_after_secs` is the upstream Retry-After header (in seconds)
    /// if present. We prefer it over body extraction for 429s.
    pub(crate) fn classify_http_error(
        status: reqwest::StatusCode,
        body: &[u8],
        retry_after_secs: Option<u64>,
    ) -> LlmError {
        let body_text = String::from_utf8_lossy(body).to_string();
        let upstream_message = extract_error_message(&body_text);

        match status.as_u16() {
            401 | 403 => LlmError::Auth,
            429 => {
                let retry_after_ms = retry_after_secs
                    .map(|s| s.saturating_mul(1_000))
                    .unwrap_or(1_000);
                LlmError::RateLimited { retry_after_ms }
            }
            // 529 is Anthropic's "overloaded" — surface as a Provider error
            // with the upstream message so retry_utils can decide.
            _ => LlmError::Provider {
                status: status.as_u16(),
                message: upstream_message,
            },
        }
    }

    fn extract_error_message(body: &str) -> String {
        // Anthropic: `{"type":"error","error":{"type":"...","message":"..."}}`
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
        crate::agent::llm::redact_body_for_error(body)
    }

    // --- Streaming ----------------------------------------------------------

    /// Stateful adapter: takes decoded Anthropic SSE events and
    /// emits our internal `StreamEvent` series.
    ///
    /// Tracks per-block accumulator state because Anthropic streams
    /// tool-use input as `input_json_delta` fragments under one
    /// `content_block` index — the full JSON is only known when
    /// `content_block_stop` arrives. For text blocks we simply
    /// pass deltas through as `StreamEvent::TextDelta`; the
    /// cumulative text is the sum.
    ///
    /// Usage tracking: `message_start` carries the input/cache
    /// counts; `message_delta` carries the **running total** of
    /// output tokens (per Anthropic docs), so we overwrite on each
    /// delta. `message_stop` triggers a `Done` emission with the
    /// final usage + stop reason.
    pub(crate) struct StreamConverter {
        model: String,
        usage: Usage,
        stop_reason: Option<String>,
        blocks: std::collections::HashMap<u32, BlockState>,
        /// Set on `message_stop` so `chat_stream`'s adapter
        /// terminates the stream with a final `Done` event.
        finished: bool,
    }

    enum BlockState {
        Text,
        ToolUse {
            id: String,
            name: String,
            json_accum: String,
        },
    }

    impl StreamConverter {
        pub(crate) fn new(default_model: &str) -> Self {
            Self {
                model: default_model.to_string(),
                usage: Usage::default(),
                stop_reason: None,
                blocks: std::collections::HashMap::new(),
                finished: false,
            }
        }

        pub(crate) fn is_finished(&self) -> bool {
            self.finished
        }

        /// Process one SSE event. Returns zero or more StreamEvents
        /// to forward downstream. Each Result lets us surface
        /// `error` SSE events as `Err(LlmError::...)` without
        /// terminating the stream — caller decides whether to stop
        /// on first error.
        pub(crate) fn process(&mut self, ev: &SseEvent) -> Vec<Result<StreamEvent>> {
            // Once message_stop has fired, the stream is logically
            // closed. Any trailing events the upstream sends (or
            // bytes we haven't yet consumed when the wrapper signals
            // EOF) are silently dropped instead of surfacing to the
            // caller as parse errors.
            if self.finished {
                return Vec::new();
            }
            // `event:` field is authoritative; `data` is JSON. We
            // also peek at the JSON's `type` to be defensive against
            // missing event headers (some upstreams omit them).
            let payload: serde_json::Value = match serde_json::from_str(&ev.data) {
                Ok(v) => v,
                Err(e) => {
                    return vec![Err(LlmError::Parse(format!("anthropic sse json: {e}")))];
                }
            };
            let kind = payload
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or(ev.event.as_str());
            match kind {
                "message_start" => self.on_message_start(&payload),
                "content_block_start" => self.on_content_block_start(&payload),
                "content_block_delta" => self.on_content_block_delta(&payload),
                "content_block_stop" => self.on_content_block_stop(&payload),
                "message_delta" => self.on_message_delta(&payload),
                "message_stop" => {
                    self.finished = true;
                    vec![Ok(self.build_done_event())]
                }
                "ping" => Vec::new(),
                "error" => vec![Err(self.parse_error_event(&payload))],
                _ => Vec::new(),
            }
        }

        fn on_message_start(&mut self, payload: &serde_json::Value) -> Vec<Result<StreamEvent>> {
            if let Some(msg) = payload.get("message") {
                if let Some(m) = msg.get("model").and_then(|v| v.as_str()) {
                    self.model = m.to_string();
                }
                if let Some(u) = msg.get("usage") {
                    self.usage.input_tokens =
                        u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    self.usage.output_tokens =
                        u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    self.usage.cache_read_tokens = u
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    self.usage.cache_write_tokens = u
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                }
            }
            Vec::new()
        }

        fn on_content_block_start(
            &mut self,
            payload: &serde_json::Value,
        ) -> Vec<Result<StreamEvent>> {
            let index = payload.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let block = match payload.get("content_block") {
                Some(b) => b,
                None => return Vec::new(),
            };
            let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match block_type {
                "text" => {
                    self.blocks.insert(index, BlockState::Text);
                    Vec::new()
                }
                "tool_use" => {
                    let id = block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    self.blocks.insert(
                        index,
                        BlockState::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            json_accum: String::new(),
                        },
                    );
                    vec![Ok(StreamEvent::ToolUseStart { id, name })]
                }
                _ => Vec::new(),
            }
        }

        fn on_content_block_delta(
            &mut self,
            payload: &serde_json::Value,
        ) -> Vec<Result<StreamEvent>> {
            let index = payload.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let delta = match payload.get("delta") {
                Some(d) => d,
                None => return Vec::new(),
            };
            let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match delta_type {
                "text_delta" => {
                    let text = delta
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if text.is_empty() {
                        Vec::new()
                    } else {
                        vec![Ok(StreamEvent::TextDelta { text })]
                    }
                }
                "input_json_delta" => {
                    let partial = delta
                        .get("partial_json")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let id_for_event = match self.blocks.get_mut(&index) {
                        Some(BlockState::ToolUse { id, json_accum, .. }) => {
                            json_accum.push_str(&partial);
                            id.clone()
                        }
                        _ => String::new(),
                    };
                    if partial.is_empty() {
                        Vec::new()
                    } else {
                        vec![Ok(StreamEvent::ToolInputDelta {
                            id: id_for_event,
                            partial_json: partial,
                        })]
                    }
                }
                // thinking_delta / signature_delta — Anthropic
                // extended-thinking. Emit nothing (we don't surface
                // thinking tokens through the public stream API today).
                _ => Vec::new(),
            }
        }

        fn on_content_block_stop(
            &mut self,
            payload: &serde_json::Value,
        ) -> Vec<Result<StreamEvent>> {
            let index = payload.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            // For tool_use: parse accumulated JSON, emit final
            // ToolUse(ToolCall). For text: nothing — text deltas
            // already streamed; the consumer concatenates.
            match self.blocks.remove(&index) {
                Some(BlockState::ToolUse {
                    id,
                    name,
                    json_accum,
                }) => {
                    let input: serde_json::Value = if json_accum.is_empty() {
                        serde_json::json!({})
                    } else {
                        match serde_json::from_str(&json_accum) {
                            Ok(v) => v,
                            Err(_) => serde_json::Value::String(json_accum),
                        }
                    };
                    vec![Ok(StreamEvent::ToolUse(ToolCall { id, name, input }))]
                }
                _ => Vec::new(),
            }
        }

        fn on_message_delta(&mut self, payload: &serde_json::Value) -> Vec<Result<StreamEvent>> {
            if let Some(d) = payload.get("delta") {
                if let Some(reason) = d.get("stop_reason").and_then(|v| v.as_str()) {
                    self.stop_reason = Some(reason.to_string());
                }
            }
            // Per Anthropic docs: usage.output_tokens here is the
            // running total. Overwrite (not accumulate).
            if let Some(u) = payload.get("usage") {
                if let Some(out) = u.get("output_tokens").and_then(|v| v.as_u64()) {
                    self.usage.output_tokens = out as u32;
                }
            }
            Vec::new()
        }

        fn parse_error_event(&self, payload: &serde_json::Value) -> LlmError {
            let err = payload.get("error");
            let kind = err
                .and_then(|e| e.get("type"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            let msg = err
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("upstream stream error")
                .to_string();
            match kind {
                "rate_limit_error" => LlmError::RateLimited {
                    retry_after_ms: 1_000,
                },
                "authentication_error" | "permission_error" => LlmError::Auth,
                "overloaded_error" => LlmError::Provider {
                    status: 529,
                    message: msg,
                },
                _ => LlmError::Provider {
                    status: 500,
                    message: msg,
                },
            }
        }

        fn build_done_event(&self) -> StreamEvent {
            let finish = match self.stop_reason.as_deref() {
                Some("end_turn") => FinishReason::Stop,
                Some("max_tokens") => FinishReason::Length,
                Some("tool_use") => FinishReason::ToolUse,
                Some("stop_sequence") => FinishReason::Stop,
                Some("refusal") => FinishReason::Refusal,
                _ => FinishReason::Stop,
            };
            StreamEvent::Done {
                finish,
                usage: self.usage.clone(),
            }
        }

    }

    #[cfg(test)]
    mod test_support {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/test/unit/agent/llm/providers/anthropic/wire_test_support.rs"
        ));
    }

    /// Bridges the response `bytes_stream()` (chunks of bytes) to
    /// our internal `Stream<Item = Result<StreamEvent, LlmError>>`.
    /// Owns the parser + converter and a small ready-event queue.
    ///
    /// Generic over the byte source so unit tests can plug a
    /// `stream::iter([Ok(bytes), ...])` instead of an HTTP body.
    pub(crate) struct AnthropicStream {
        bytes: BoxStream<'static, std::result::Result<bytes::Bytes, reqwest::Error>>,
        parser: crate::agent::llm::sse::SseParser,
        converter: StreamConverter,
        pending: std::collections::VecDeque<Result<StreamEvent>>,
        bytes_done: bool,
        total_bytes: usize,
        pool: Option<Arc<crate::agent::llm::credential_pool::Pool>>,
        lease: Option<crate::agent::llm::credential_pool::Lease>,
        accounted: bool,
    }

    impl AnthropicStream {
        pub(crate) fn new<S>(
            bytes: S,
            default_model: &str,
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
                converter: StreamConverter::new(default_model),
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
                let events = self.converter.process(&sse);
                self.pending.extend(events);
            }
        }

        /// Translate an SSE parser-level overflow into a stream error
        /// the caller can surface as `LlmError::UpstreamMalformed`.
        /// Once called, the byte source is considered drained so the
        /// stream terminates after this single error.
        fn surface_sse_overflow(&mut self, e: crate::agent::llm::sse::SseOverflow) {
            self.pending.push_back(Err(LlmError::UpstreamMalformed(
                format!("anthropic stream: {e}"),
            )));
            self.bytes_done = true;
            self.report_failure_once(
                crate::agent::llm::credential_pool::FailureClass::Transient,
            );
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

        fn report_failure_once(
            &mut self,
            cls: crate::agent::llm::credential_pool::FailureClass,
        ) {
            if self.accounted {
                return;
            }
            self.accounted = true;
            if let (Some(p), Some(l)) = (&self.pool, &self.lease) {
                p.report_failure(l, cls);
            }
        }
    }

    impl futures_util::Stream for AnthropicStream {
        type Item = Result<StreamEvent>;

        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            use std::task::Poll;
            loop {
                if let Some(ev) = self.pending.pop_front() {
                    // MEDIUM-9: credit / charge the lease only when
                    // the body actually finishes. The terminal `Done`
                    // event means the stream completed successfully;
                    // any prior error is charged as Transient.
                    if matches!(ev, Ok(StreamEvent::Done { .. })) {
                        self.report_success_once();
                    } else if ev.is_err() {
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
                            self.surface_sse_overflow(e);
                            continue;
                        }
                        self.drain_parser();
                        if !self.converter.is_finished() {
                            self.pending.push_back(Err(LlmError::UpstreamMalformed(
                                "anthropic stream ended before message_stop".into(),
                            )));
                            self.report_failure_once(
                                crate::agent::llm::credential_pool::FailureClass::Transient,
                            );
                        }
                        self.bytes_done = true;
                        continue;
                    }
                    Poll::Ready(Some(Ok(chunk))) => {
                        self.total_bytes =
                            self.total_bytes.saturating_add(chunk.len());
                        if self.total_bytes > crate::agent::llm::MAX_STREAM_TOTAL_BYTES {
                            self.pending.push_back(Err(LlmError::UpstreamMalformed(
                                format!(
                                    "anthropic stream exceeded {} bytes",
                                    crate::agent::llm::MAX_STREAM_TOTAL_BYTES
                                ),
                            )));
                            self.bytes_done = true;
                            self.report_failure_once(
                                crate::agent::llm::credential_pool::FailureClass::Transient,
                            );
                            continue;
                        }
                        if let Err(e) = self.parser.feed(&chunk) {
                            self.surface_sse_overflow(e);
                            continue;
                        }
                        self.drain_parser();
                        // If converter signalled finish, drop any
                        // remaining buffered bytes.
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

pub fn is_alias(name: &str) -> bool {
    name == PROVIDER_NAME
}

pub fn build_provider(model: &str, agent: &AgentConfig) -> Arc<dyn Provider> {
    Arc::new(AnthropicProvider::from_agent_config(model, agent))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/providers/anthropic.rs"
    ));
}
