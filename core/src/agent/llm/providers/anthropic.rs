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
use futures_util::stream::{self, BoxStream, StreamExt};
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
            .field("extra_headers", &self.extra_headers.keys().collect::<Vec<_>>())
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
        let mut builder = reqwest::Client::builder().user_agent(concat!(
            "cos-agent/",
            env!("CARGO_PKG_VERSION")
        ));
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
        let bytes = match resp.bytes().await {
            Ok(b) => b,
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

        if !status.is_success() {
            let err = wire::classify_http_error(status, &bytes, retry_after_secs);
            if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                let body_str = std::str::from_utf8(&bytes).unwrap_or("");
                let cls = crate::agent::llm::error_classifier::classify(
                    status.as_u16(),
                    body_str,
                );
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
            let bytes = resp.bytes().await.map_err(LlmError::Transport)?;
            let err = wire::classify_http_error(status, &bytes, retry_after_secs);
            if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                let body_str = std::str::from_utf8(&bytes).unwrap_or("");
                let cls = crate::agent::llm::error_classifier::classify(
                    status.as_u16(),
                    body_str,
                );
                pool.report_failure(l, cls);
            }
            return Err(err);
        }

        if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
            // Success-on-headers — credit the lease so cooldowns
            // clear. Subsequent body errors don't penalise (the
            // upstream did accept the request).
            pool.report_success(l);
        }

        let bytes_stream = resp.bytes_stream();
        let model = self.cfg.model.clone();
        let stream = wire::AnthropicStream::new(bytes_stream, &model);
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
            if let serde_json::Value::Object(extra) = &working.extra {
                for (k, v) in extra {
                    obj.insert(k.clone(), v.clone());
                }
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

    fn message_to_json(m: &crate::agent::llm::Message, cache_breakpoint: bool) -> serde_json::Value {
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
            m.content.iter().map(content_block_to_json).collect();

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

    fn content_block_to_json(b: &ContentBlock) -> serde_json::Value {
        match b {
            ContentBlock::Text { text } => serde_json::json!({
                "type": "text",
                "text": text,
            }),
            ContentBlock::ToolUse { id, name, input } => serde_json::json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }),
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
                obj
            }
            ContentBlock::Image { media_type, data } => serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type,
                    "data": data,
                },
            }),
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

    pub(crate) fn response_to_chat(
        resp: Response,
        fallback_model: &str,
    ) -> Result<ChatResponse> {
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
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        input,
                    });
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
                return msg.to_string();
            }
            if let Some(msg) = v.get("message").and_then(|m| m.as_str()) {
                return msg.to_string();
            }
        }
        body.chars().take(500).collect()
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
        pub(crate) fn process(
            &mut self,
            ev: &SseEvent,
        ) -> Vec<Result<StreamEvent>> {
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
            let payload: serde_json::Value =
                match serde_json::from_str(&ev.data) {
                    Ok(v) => v,
                    Err(e) => {
                        return vec![Err(LlmError::Parse(format!(
                            "anthropic sse json: {e}"
                        )))];
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

        fn on_message_start(
            &mut self,
            payload: &serde_json::Value,
        ) -> Vec<Result<StreamEvent>> {
            if let Some(msg) = payload.get("message") {
                if let Some(m) = msg.get("model").and_then(|v| v.as_str()) {
                    self.model = m.to_string();
                }
                if let Some(u) = msg.get("usage") {
                    self.usage.input_tokens = u
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    self.usage.output_tokens = u
                        .get("output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
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
            let index = payload
                .get("index")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let block = match payload.get("content_block") {
                Some(b) => b,
                None => return Vec::new(),
            };
            let block_type = block
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("");
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
            let index = payload
                .get("index")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let delta = match payload.get("delta") {
                Some(d) => d,
                None => return Vec::new(),
            };
            let delta_type = delta
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("");
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
                        Some(BlockState::ToolUse {
                            id, json_accum, ..
                        }) => {
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
            let index = payload
                .get("index")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
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
                    vec![Ok(StreamEvent::ToolUse(ToolCall {
                        id,
                        name,
                        input,
                    }))]
                }
                _ => Vec::new(),
            }
        }

        fn on_message_delta(
            &mut self,
            payload: &serde_json::Value,
        ) -> Vec<Result<StreamEvent>> {
            if let Some(d) = payload.get("delta") {
                if let Some(reason) =
                    d.get("stop_reason").and_then(|v| v.as_str())
                {
                    self.stop_reason = Some(reason.to_string());
                }
            }
            // Per Anthropic docs: usage.output_tokens here is the
            // running total. Overwrite (not accumulate).
            if let Some(u) = payload.get("usage") {
                if let Some(out) =
                    u.get("output_tokens").and_then(|v| v.as_u64())
                {
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
                "rate_limit_error" => {
                    LlmError::RateLimited { retry_after_ms: 1_000 }
                }
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

        #[cfg(test)]
        pub(crate) fn debug_model(&self) -> &str {
            &self.model
        }

        #[cfg(test)]
        pub(crate) fn debug_usage(&self) -> &Usage {
            &self.usage
        }
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
    }

    impl AnthropicStream {
        pub(crate) fn new<S>(bytes: S, default_model: &str) -> Self
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
            }
        }

        fn drain_parser(&mut self) {
            while let Some(sse) = self.parser.pop_event() {
                let events = self.converter.process(&sse);
                self.pending.extend(events);
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
                    return Poll::Ready(Some(ev));
                }
                if self.bytes_done {
                    return Poll::Ready(None);
                }
                match std::pin::Pin::new(&mut self.bytes).poll_next(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => {
                        self.parser.finish();
                        self.drain_parser();
                        self.bytes_done = true;
                        continue;
                    }
                    Poll::Ready(Some(Ok(chunk))) => {
                        self.parser.feed(&chunk);
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
    use super::*;
    use crate::agent::llm::{Message, Role, Tool};

    fn cfg() -> AgentConfig {
        AgentConfig::default()
    }

    fn req_text(text: &str) -> ChatRequest {
        ChatRequest {
            model: "claude-3-5-sonnet-20241022".into(),
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
    fn default_base_url_is_anthropic_com() {
        assert!(default_base_url().starts_with("https://api.anthropic.com"));
    }

    #[test]
    fn config_uses_override_when_set() {
        let mut c = cfg();
        c.base_url = Some("https://my.proxy".into());
        let ac = AnthropicConfig::from_agent_config("claude-3-5-haiku-20241022", &c);
        assert_eq!(ac.base_url, "https://my.proxy");
    }

    #[test]
    fn config_strips_trailing_slash() {
        let mut c = cfg();
        c.base_url = Some("https://my.proxy/".into());
        let ac = AnthropicConfig::from_agent_config("claude-3-5-haiku-20241022", &c);
        assert_eq!(ac.base_url, "https://my.proxy");
    }

    #[test]
    fn empty_base_url_falls_back_to_default() {
        let mut c = cfg();
        c.base_url = Some(String::new());
        let ac = AnthropicConfig::from_agent_config("claude-3-5-haiku-20241022", &c);
        assert!(ac.base_url.starts_with("https://api.anthropic.com"));
    }

    #[test]
    fn endpoint_appends_messages_path() {
        let mut c = cfg();
        c.base_url = Some("https://api.anthropic.com".into());
        let provider = AnthropicProvider::from_agent_config("claude-3-5-haiku-20241022", &c);
        assert_eq!(
            provider.endpoint(),
            "https://api.anthropic.com/v1/messages"
        );
    }

    // ---- is_configured ---------------------------------------------------

    #[test]
    fn is_configured_true_when_api_key_present() {
        let mut c = cfg();
        c.api_key_env = Some("COS_TEST_ANTHROPIC_KEY_X".into());
        std::env::set_var("COS_TEST_ANTHROPIC_KEY_X", "sk-ant-x");
        let p = AnthropicProvider::from_agent_config("claude-3-5-haiku-20241022", &c);
        assert!(p.is_configured());
        std::env::remove_var("COS_TEST_ANTHROPIC_KEY_X");
    }

    #[test]
    fn is_configured_false_without_key() {
        let p = AnthropicProvider::from_agent_config("claude-3-5-haiku-20241022", &cfg());
        assert!(!p.is_configured());
    }

    // ---- request body serialisation --------------------------------------

    #[test]
    fn builds_minimal_chat_body() {
        let r = req_text("hello");
        let body = wire::build_request_body(&r, "claude-3-5-sonnet-20241022", false);
        assert_eq!(body["model"], "claude-3-5-sonnet-20241022");
        // System hoisted to top level (NOT in messages).
        assert_eq!(body["system"], "you are helpful");
        // Messages contains only the user turn — no system message.
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        // Content is an array of blocks, not a string.
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hello");
        assert_eq!(body["max_tokens"], 64);
        assert!(body.get("tools").is_none(), "no tools means no tools field");
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn always_emits_max_tokens_even_when_caller_omits() {
        let mut r = req_text("hi");
        r.max_tokens = None;
        let body = wire::build_request_body(&r, "claude-3-5-haiku-20241022", false);
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn body_omits_system_when_empty() {
        let mut r = req_text("hi");
        r.system = Some(String::new());
        let body = wire::build_request_body(&r, "claude-3-5-haiku-20241022", false);
        assert!(body.get("system").is_none());
    }

    #[test]
    fn body_omits_system_when_none() {
        let mut r = req_text("hi");
        r.system = None;
        let body = wire::build_request_body(&r, "claude-3-5-haiku-20241022", false);
        assert!(body.get("system").is_none());
    }

    #[test]
    fn body_includes_tools_as_flat_objects() {
        let mut r = req_text("call tool");
        r.tools = vec![Tool {
            name: "echo".into(),
            description: "echo it".into(),
            input_schema: serde_json::json!({"type":"object","properties":{}}),
        }];
        let body = wire::build_request_body(&r, "claude-3-5-haiku-20241022", false);
        // No nested "function" wrapper — flat object.
        assert_eq!(body["tools"][0]["name"], "echo");
        assert_eq!(body["tools"][0]["description"], "echo it");
        assert!(body["tools"][0].get("function").is_none());
        assert_eq!(body["tool_choice"]["type"], "auto");
    }

    #[test]
    fn body_marks_stream_when_requested() {
        let r = req_text("hi");
        let body = wire::build_request_body(&r, "claude-3-5-haiku-20241022", true);
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn body_renders_assistant_tool_use_as_content_block() {
        let mut r = req_text("ignored");
        r.messages.push(crate::agent::llm::Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "toolu_01".into(),
                name: "echo".into(),
                input: serde_json::json!({"text":"hi"}),
            }],
        });
        let body = wire::build_request_body(&r, "claude-3-5-haiku-20241022", false);
        let asst = &body["messages"][1];
        assert_eq!(asst["role"], "assistant");
        assert_eq!(asst["content"][0]["type"], "tool_use");
        assert_eq!(asst["content"][0]["id"], "toolu_01");
        assert_eq!(asst["content"][0]["name"], "echo");
        assert_eq!(asst["content"][0]["input"]["text"], "hi");
    }

    #[test]
    fn body_renders_tool_result_as_user_block() {
        let mut r = req_text("ignored");
        r.messages.push(crate::agent::llm::Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_01".into(),
                is_error: false,
                content: "{\"ok\":true}".into(),
            }],
        });
        let body = wire::build_request_body(&r, "claude-3-5-haiku-20241022", false);
        // Tool result is folded into a user message with content array.
        let msg = &body["messages"][1];
        assert_eq!(msg["role"], "user");
        assert_eq!(msg["content"][0]["type"], "tool_result");
        assert_eq!(msg["content"][0]["tool_use_id"], "toolu_01");
        assert_eq!(msg["content"][0]["content"], "{\"ok\":true}");
        assert!(
            msg["content"][0].get("is_error").is_none(),
            "is_error should be omitted when false"
        );
    }

    #[test]
    fn body_renders_tool_result_with_is_error_when_set() {
        let mut r = req_text("ignored");
        r.messages.push(crate::agent::llm::Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_99".into(),
                is_error: true,
                content: "oops".into(),
            }],
        });
        let body = wire::build_request_body(&r, "claude-3-5-haiku-20241022", false);
        let msg = &body["messages"][1];
        assert_eq!(msg["content"][0]["is_error"], true);
    }

    #[test]
    fn body_emits_stop_sequences_under_anthropic_key() {
        let mut r = req_text("hi");
        r.stop_sequences = vec!["END".into(), "STOP".into()];
        let body = wire::build_request_body(&r, "claude-3-5-haiku-20241022", false);
        // Anthropic uses "stop_sequences", not OpenAI's "stop".
        let arr = body["stop_sequences"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], "END");
        assert!(body.get("stop").is_none());
    }

    #[test]
    fn body_merges_extras() {
        let mut r = req_text("hi");
        r.extra = serde_json::json!({"metadata": {"user_id": "u-1"}});
        let body = wire::build_request_body(&r, "claude-3-5-haiku-20241022", false);
        assert_eq!(body["metadata"]["user_id"], "u-1");
    }

    #[test]
    fn body_renders_image_content_block() {
        let mut r = req_text("ignored");
        r.messages.push(crate::agent::llm::Message {
            role: Role::User,
            content: vec![ContentBlock::Image {
                media_type: "image/png".into(),
                data: "iVBORw0KGgo=".into(),
            }],
        });
        let body = wire::build_request_body(&r, "claude-3-5-haiku-20241022", false);
        let img = &body["messages"][1]["content"][0];
        assert_eq!(img["type"], "image");
        assert_eq!(img["source"]["type"], "base64");
        assert_eq!(img["source"]["media_type"], "image/png");
        assert_eq!(img["source"]["data"], "iVBORw0KGgo=");
    }

    #[test]
    fn tool_choice_required_maps_to_any() {
        let mut r = req_text("call");
        r.tools = vec![Tool {
            name: "echo".into(),
            description: "".into(),
            input_schema: serde_json::json!({}),
        }];
        r.tool_choice = ToolChoice::Required;
        let body = wire::build_request_body(&r, "claude-3-5-haiku-20241022", false);
        assert_eq!(body["tool_choice"]["type"], "any");
    }

    #[test]
    fn tool_choice_specific_tool_includes_name() {
        let mut r = req_text("call");
        r.tools = vec![Tool {
            name: "echo".into(),
            description: "".into(),
            input_schema: serde_json::json!({}),
        }];
        r.tool_choice = ToolChoice::Tool {
            name: "echo".into(),
        };
        let body = wire::build_request_body(&r, "claude-3-5-haiku-20241022", false);
        assert_eq!(body["tool_choice"]["type"], "tool");
        assert_eq!(body["tool_choice"]["name"], "echo");
    }

    // ---- response parsing ------------------------------------------------

    #[test]
    fn parses_simple_text_response() {
        let raw = serde_json::json!({
            "id": "msg_01",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-5-sonnet-20241022",
            "content": [{"type": "text", "text": "hello there"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 3}
        });
        let resp: wire::Response = serde_json::from_value(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        assert_eq!(chat.model, "claude-3-5-sonnet-20241022");
        assert_eq!(chat.content.len(), 1);
        match &chat.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello there"),
            _ => panic!("expected text"),
        }
        assert!(chat.tool_calls.is_empty());
        assert!(matches!(chat.finish_reason, FinishReason::Stop));
        assert_eq!(chat.usage.input_tokens, 10);
        assert_eq!(chat.usage.output_tokens, 3);
    }

    #[test]
    fn parses_tool_use_response() {
        let raw = serde_json::json!({
            "id": "msg_02",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-5-sonnet-20241022",
            "content": [
                {"type": "text", "text": "let me check"},
                {"type": "tool_use", "id": "toolu_42", "name": "lookup",
                 "input": {"query": "weather"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 20, "output_tokens": 12}
        });
        let resp: wire::Response = serde_json::from_value(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        assert_eq!(chat.content.len(), 2);
        assert_eq!(chat.tool_calls.len(), 1);
        assert_eq!(chat.tool_calls[0].id, "toolu_42");
        assert_eq!(chat.tool_calls[0].name, "lookup");
        assert_eq!(chat.tool_calls[0].input["query"], "weather");
        assert!(matches!(chat.finish_reason, FinishReason::ToolUse));
    }

    #[test]
    fn parses_response_with_unknown_content_block() {
        // Forward-compat: thinking blocks etc. should be skipped, not error.
        let raw = serde_json::json!({
            "id": "msg_03",
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-4-20250514",
            "content": [
                {"type": "thinking", "thinking": "let me reason..."},
                {"type": "text", "text": "answer"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 2}
        });
        let resp: wire::Response = serde_json::from_value(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        assert_eq!(chat.content.len(), 1, "thinking block should be skipped");
        match &chat.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "answer"),
            _ => panic!("expected text after skipping thinking"),
        }
    }

    #[test]
    fn finish_reason_max_tokens_maps_to_length() {
        let raw = serde_json::json!({
            "model": "claude-3-5-sonnet-20241022",
            "content": [{"type": "text", "text": "..."}],
            "stop_reason": "max_tokens",
            "usage": {"input_tokens": 5, "output_tokens": 64}
        });
        let resp: wire::Response = serde_json::from_value(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        assert!(matches!(chat.finish_reason, FinishReason::Length));
    }

    #[test]
    fn finish_reason_stop_sequence_maps_to_stop() {
        let raw = serde_json::json!({
            "model": "claude-3-5-sonnet-20241022",
            "content": [{"type": "text", "text": "."}],
            "stop_reason": "stop_sequence",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let resp: wire::Response = serde_json::from_value(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        assert!(matches!(chat.finish_reason, FinishReason::Stop));
    }

    #[test]
    fn parses_cache_token_fields() {
        let raw = serde_json::json!({
            "model": "claude-3-5-sonnet-20241022",
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 5,
                "output_tokens": 3,
                "cache_read_input_tokens": 1024,
                "cache_creation_input_tokens": 256
            }
        });
        let resp: wire::Response = serde_json::from_value(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        assert_eq!(chat.usage.cache_read_tokens, 1024);
        assert_eq!(chat.usage.cache_write_tokens, 256);
    }

    // ---- error classification --------------------------------------------

    #[test]
    fn classify_401_is_auth() {
        let err = wire::classify_http_error(
            reqwest::StatusCode::UNAUTHORIZED,
            br#"{"type":"error","error":{"type":"authentication_error","message":"bad key"}}"#,
            None,
        );
        assert!(matches!(err, LlmError::Auth));
    }

    #[test]
    fn classify_429_uses_retry_after_header_when_present() {
        let err = wire::classify_http_error(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            br#"{"type":"error","error":{"message":"slow down"}}"#,
            Some(7),
        );
        match err {
            LlmError::RateLimited { retry_after_ms } => assert_eq!(retry_after_ms, 7_000),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn classify_429_falls_back_to_1s_when_no_header() {
        let err = wire::classify_http_error(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            br#"{"type":"error","error":{"message":"slow down"}}"#,
            None,
        );
        match err {
            LlmError::RateLimited { retry_after_ms } => assert_eq!(retry_after_ms, 1_000),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn classify_overloaded_529_surfaces_as_provider() {
        let err = wire::classify_http_error(
            reqwest::StatusCode::from_u16(529).unwrap(),
            br#"{"type":"error","error":{"type":"overloaded_error","message":"servers busy"}}"#,
            None,
        );
        match err {
            LlmError::Provider { status, message } => {
                assert_eq!(status, 529);
                assert!(message.contains("servers busy"));
            }
            other => panic!("expected Provider, got {other:?}"),
        }
    }

    #[test]
    fn extract_error_message_from_anthropic_envelope() {
        let err = wire::classify_http_error(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            br#"{"type":"error","error":{"type":"api_error","message":"upstream borked"}}"#,
            None,
        );
        match err {
            LlmError::Provider { status, message } => {
                assert_eq!(status, 500);
                assert_eq!(message, "upstream borked");
            }
            other => panic!("expected Provider, got {other:?}"),
        }
    }

    #[test]
    fn registry_alias_check() {
        assert!(is_alias("anthropic"));
        assert!(!is_alias("openai"));
        assert!(!is_alias(""));
    }

    // --- Prompt cache wire integration tests --------------------------

    #[test]
    fn body_no_cache_control_when_no_markers() {
        let r = req_text("hi");
        let body = wire::build_request_body(&r, "claude-3-5-sonnet-20241022", false);
        let serialised = serde_json::to_string(&body).unwrap();
        assert!(
            !serialised.contains("cache_control"),
            "no markers should mean no cache_control on the wire"
        );
    }

    #[test]
    fn body_breakpoint_attaches_cache_control_to_last_block_of_message() {
        use crate::agent::prompt::caching;
        let mut r = req_text("hi");
        r.messages.push(crate::agent::llm::Message::assistant_text(
            "thinking out loud",
        ));
        r.messages
            .push(crate::agent::llm::Message::user_text("follow-up"));
        // Mark message at index 1 (the assistant message) as cached.
        caching::mark_breakpoint(&mut r, 1).unwrap();
        let body = wire::build_request_body(&r, "claude-3-5-sonnet-20241022", false);
        let msg1 = &body["messages"][1];
        let last_block = &msg1["content"][0];
        assert_eq!(last_block["cache_control"]["type"], "ephemeral");
        // Other messages have no cache_control.
        let msg0 = &body["messages"][0];
        assert!(msg0["content"][0].get("cache_control").is_none());
        let msg2 = &body["messages"][2];
        assert!(msg2["content"][0].get("cache_control").is_none());
    }

    #[test]
    fn body_cache_system_promotes_string_to_block_array() {
        use crate::agent::prompt::caching;
        let mut r = req_text("hi");
        r.system = Some("be helpful".into());
        caching::mark_system_cached(&mut r);
        let body = wire::build_request_body(&r, "claude-3-5-sonnet-20241022", false);
        let sys = &body["system"];
        assert!(sys.is_array(), "system should be an array when cached");
        let first = &sys[0];
        assert_eq!(first["type"], "text");
        assert_eq!(first["text"], "be helpful");
        assert_eq!(first["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn body_cache_tools_attaches_cache_control_to_last_tool() {
        use crate::agent::prompt::caching;
        let mut r = req_text("hi");
        r.tools = vec![
            Tool {
                name: "first".into(),
                description: "".into(),
                input_schema: serde_json::json!({}),
            },
            Tool {
                name: "second".into(),
                description: "".into(),
                input_schema: serde_json::json!({}),
            },
        ];
        caching::mark_tools_cached(&mut r);
        let body = wire::build_request_body(&r, "claude-3-5-sonnet-20241022", false);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        // First tool: no cache_control.
        assert!(tools[0].get("cache_control").is_none());
        // Last tool: cache_control attached.
        assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn body_cache_markers_do_not_leak_into_extras() {
        use crate::agent::prompt::caching;
        let mut r = req_text("hi");
        caching::mark_breakpoint(&mut r, 0).unwrap();
        caching::mark_system_cached(&mut r);
        let body = wire::build_request_body(&r, "claude-3-5-sonnet-20241022", false);
        let serialised = serde_json::to_string(&body).unwrap();
        assert!(!serialised.contains("__cache_breakpoints"));
        assert!(!serialised.contains("__cache_system"));
        assert!(!serialised.contains("__cache_tools"));
    }

    #[test]
    fn body_cache_markers_preserve_non_cache_extras() {
        use crate::agent::prompt::caching;
        let mut r = req_text("hi");
        r.extra = serde_json::json!({"metadata": {"user_id": "u-7"}});
        caching::mark_breakpoint(&mut r, 0).unwrap();
        let body = wire::build_request_body(&r, "claude-3-5-sonnet-20241022", false);
        // metadata still present at top level.
        assert_eq!(body["metadata"]["user_id"], "u-7");
        // breakpoint applied.
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn body_breakpoint_does_not_mutate_caller_request() {
        use crate::agent::prompt::caching;
        let mut r = req_text("hi");
        caching::mark_breakpoint(&mut r, 0).unwrap();
        let _ = wire::build_request_body(&r, "claude-3-5-sonnet-20241022", false);
        // Marker still present on the original request — wire builder
        // works on a clone.
        assert_eq!(caching::get_breakpoints(&r), vec![0]);
    }

    #[test]
    fn body_out_of_range_breakpoint_dropped_silently() {
        use crate::agent::prompt::caching;
        let mut r = req_text("hi"); // 1 message
        // Mark index 99 as a breakpoint — bigger than messages.len().
        caching::set_breakpoints(&mut r, vec![99]);
        let body = wire::build_request_body(&r, "claude-3-5-sonnet-20241022", false);
        let serialised = serde_json::to_string(&body).unwrap();
        assert!(
            !serialised.contains("cache_control"),
            "out-of-range breakpoint should not produce cache_control"
        );
    }

    #[test]
    fn body_cache_system_with_empty_system_no_op() {
        use crate::agent::prompt::caching;
        let mut r = req_text("hi");
        r.system = None;
        caching::mark_system_cached(&mut r);
        let body = wire::build_request_body(&r, "claude-3-5-sonnet-20241022", false);
        // No system field on the wire.
        assert!(body.get("system").is_none());
    }

    // ---- credential pool wiring ------------------------------------------

    #[test]
    fn anthropic_no_pool_when_neither_plural_field_set() {
        let c = AgentConfig::default();
        let ac = AnthropicConfig::from_agent_config("claude-3-5-haiku-20241022", &c);
        assert!(ac.pool.is_none());
    }

    #[test]
    fn anthropic_pool_built_from_envs() {
        std::env::set_var("COS_TEST_ANTH_POOL_A", "sk-ant-aaa");
        std::env::set_var("COS_TEST_ANTH_POOL_B", "sk-ant-bbb");
        let mut c = AgentConfig::default();
        c.api_key_envs = vec![
            "COS_TEST_ANTH_POOL_A".into(),
            "COS_TEST_ANTH_POOL_B".into(),
        ];
        let ac = AnthropicConfig::from_agent_config("claude-3-5-haiku-20241022", &c);
        std::env::remove_var("COS_TEST_ANTH_POOL_A");
        std::env::remove_var("COS_TEST_ANTH_POOL_B");
        let pool = ac.pool.expect("pool should be built");
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn anthropic_is_configured_true_with_pool_only() {
        std::env::set_var("COS_TEST_ANTH_POOL_ICONFIG", "sk-ant-x");
        let mut c = AgentConfig::default();
        c.api_key_envs = vec!["COS_TEST_ANTH_POOL_ICONFIG".into()];
        let ac = AnthropicConfig::from_agent_config("claude-3-5-haiku-20241022", &c);
        std::env::remove_var("COS_TEST_ANTH_POOL_ICONFIG");
        let provider = AnthropicProvider::new(ac);
        assert!(provider.is_configured());
    }

    // ---- StreamConverter (SSE event → StreamEvent) -----------------------

    mod stream_converter {
        use super::*;
        use crate::agent::llm::sse::SseEvent;
        use crate::agent::llm::types::FinishReason;

        fn ev(name: &str, data_json: &str) -> SseEvent {
            SseEvent {
                event: name.to_string(),
                data: data_json.to_string(),
            }
        }

        fn run<'a>(
            conv: &mut wire::StreamConverter,
            events: impl IntoIterator<Item = &'a SseEvent>,
        ) -> Vec<Result<StreamEvent>> {
            let mut out = Vec::new();
            for e in events {
                out.extend(conv.process(e));
            }
            out
        }

        #[test]
        fn message_start_captures_model_and_input_tokens() {
            let mut c = wire::StreamConverter::new("fallback");
            let events = vec![ev(
                "message_start",
                r#"{"type":"message_start","message":{"id":"m1","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":42,"output_tokens":1,"cache_read_input_tokens":3,"cache_creation_input_tokens":5}}}"#,
            )];
            let out = run(&mut c, events.iter());
            assert!(out.is_empty(), "message_start emits nothing downstream");
            assert_eq!(c.debug_model(), "claude-3-5-sonnet-20241022");
            let u = c.debug_usage();
            assert_eq!(u.input_tokens, 42);
            assert_eq!(u.output_tokens, 1);
            assert_eq!(u.cache_read_tokens, 3);
            assert_eq!(u.cache_write_tokens, 5);
        }

        #[test]
        fn text_only_message_yields_text_deltas_then_done() {
            let mut c = wire::StreamConverter::new("claude-x");
            let events = vec![
                ev(
                    "message_start",
                    r#"{"type":"message_start","message":{"model":"claude-3-5-haiku-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#,
                ),
                ev(
                    "content_block_start",
                    r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
                ),
                ev(
                    "content_block_delta",
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#,
                ),
                ev(
                    "content_block_delta",
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#,
                ),
                ev(
                    "content_block_delta",
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"!"}}"#,
                ),
                ev("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
                ev(
                    "message_delta",
                    r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
                ),
                ev("message_stop", r#"{"type":"message_stop"}"#),
            ];
            let out: Vec<StreamEvent> = run(&mut c, events.iter())
                .into_iter()
                .map(|r| r.expect("ok"))
                .collect();

            // Expect: 3 TextDelta then Done.
            assert_eq!(out.len(), 4, "got: {out:?}");
            match &out[0] {
                StreamEvent::TextDelta { text } => assert_eq!(text, "Hel"),
                e => panic!("want TextDelta, got {e:?}"),
            }
            match &out[1] {
                StreamEvent::TextDelta { text } => assert_eq!(text, "lo"),
                e => panic!("want TextDelta, got {e:?}"),
            }
            match &out[2] {
                StreamEvent::TextDelta { text } => assert_eq!(text, "!"),
                e => panic!("want TextDelta, got {e:?}"),
            }
            match &out[3] {
                StreamEvent::Done { finish, usage } => {
                    assert!(matches!(finish, FinishReason::Stop));
                    // message_delta usage is running total → overwrite.
                    assert_eq!(usage.output_tokens, 7);
                    assert_eq!(usage.input_tokens, 10);
                }
                e => panic!("want Done, got {e:?}"),
            }
            assert!(c.is_finished());
        }

        #[test]
        fn tool_use_assembles_input_json_and_emits_tool_use() {
            let mut c = wire::StreamConverter::new("claude-x");
            let events = vec![
                ev(
                    "message_start",
                    r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":0}}}"#,
                ),
                ev(
                    "content_block_start",
                    r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"calc","input":{}}}"#,
                ),
                ev(
                    "content_block_delta",
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}"#,
                ),
                ev(
                    "content_block_delta",
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"42}"}}"#,
                ),
                ev("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
                ev(
                    "message_delta",
                    r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}}"#,
                ),
                ev("message_stop", r#"{"type":"message_stop"}"#),
            ];
            let out: Vec<StreamEvent> = run(&mut c, events.iter())
                .into_iter()
                .map(|r| r.expect("ok"))
                .collect();

            // Expect: ToolUseStart, ToolInputDelta×2, ToolUse, Done.
            assert_eq!(out.len(), 5, "got: {out:?}");
            match &out[0] {
                StreamEvent::ToolUseStart { id, name } => {
                    assert_eq!(id, "toolu_1");
                    assert_eq!(name, "calc");
                }
                e => panic!("want ToolUseStart, got {e:?}"),
            }
            match &out[1] {
                StreamEvent::ToolInputDelta { id, partial_json } => {
                    assert_eq!(id, "toolu_1");
                    assert_eq!(partial_json, "{\"a\":");
                }
                e => panic!("want ToolInputDelta, got {e:?}"),
            }
            match &out[3] {
                StreamEvent::ToolUse(call) => {
                    assert_eq!(call.id, "toolu_1");
                    assert_eq!(call.name, "calc");
                    assert_eq!(call.input["a"], 42);
                }
                e => panic!("want ToolUse, got {e:?}"),
            }
            match &out[4] {
                StreamEvent::Done { finish, .. } => {
                    assert!(matches!(finish, FinishReason::ToolUse));
                }
                e => panic!("want Done, got {e:?}"),
            }
        }

        #[test]
        fn tool_use_with_unparseable_json_falls_back_to_string() {
            let mut c = wire::StreamConverter::new("m");
            let events = vec![
                ev(
                    "content_block_start",
                    r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"n"}}"#,
                ),
                ev(
                    "content_block_delta",
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"not-json"}}"#,
                ),
                ev("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            ];
            let out: Vec<StreamEvent> = run(&mut c, events.iter())
                .into_iter()
                .map(|r| r.expect("ok"))
                .collect();
            // Last one should be ToolUse with input as string fallback.
            let last = out.last().unwrap();
            match last {
                StreamEvent::ToolUse(call) => {
                    assert_eq!(call.input.as_str(), Some("not-json"));
                }
                e => panic!("want ToolUse, got {e:?}"),
            }
        }

        #[test]
        fn ping_events_are_skipped() {
            let mut c = wire::StreamConverter::new("m");
            let out = run(&mut c, [&ev("ping", r#"{"type":"ping"}"#)]);
            assert!(out.is_empty());
            assert!(!c.is_finished());
        }

        #[test]
        fn extended_thinking_deltas_are_silently_dropped() {
            let mut c = wire::StreamConverter::new("m");
            let events = vec![
                ev(
                    "content_block_start",
                    r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
                ),
                ev(
                    "content_block_delta",
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"i wonder..."}}"#,
                ),
                ev(
                    "content_block_delta",
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}}"#,
                ),
                ev("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            ];
            let out = run(&mut c, events.iter());
            assert!(out.is_empty(), "thinking should not surface: {out:?}");
        }

        #[test]
        fn malformed_json_yields_parse_error_but_does_not_terminate() {
            let mut c = wire::StreamConverter::new("m");
            let bad = ev("content_block_delta", "{not json");
            let out = c.process(&bad);
            assert_eq!(out.len(), 1);
            assert!(matches!(out[0], Err(LlmError::Parse(_))));
            assert!(!c.is_finished());
        }

        #[test]
        fn rate_limit_error_event_maps_to_rate_limited() {
            let mut c = wire::StreamConverter::new("m");
            let e = ev(
                "error",
                r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#,
            );
            let out = c.process(&e);
            assert_eq!(out.len(), 1);
            assert!(matches!(out[0], Err(LlmError::RateLimited { .. })));
        }

        #[test]
        fn auth_error_event_maps_to_auth() {
            let mut c = wire::StreamConverter::new("m");
            let e = ev(
                "error",
                r#"{"type":"error","error":{"type":"authentication_error","message":"bad key"}}"#,
            );
            let out = c.process(&e);
            assert!(matches!(out[0], Err(LlmError::Auth)));
        }

        #[test]
        fn permission_error_event_also_maps_to_auth() {
            let mut c = wire::StreamConverter::new("m");
            let e = ev(
                "error",
                r#"{"type":"error","error":{"type":"permission_error","message":"forbidden"}}"#,
            );
            let out = c.process(&e);
            assert!(matches!(out[0], Err(LlmError::Auth)));
        }

        #[test]
        fn overloaded_error_event_maps_to_provider_529() {
            let mut c = wire::StreamConverter::new("m");
            let e = ev(
                "error",
                r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#,
            );
            let out = c.process(&e);
            match &out[0] {
                Err(LlmError::Provider { status, .. }) => assert_eq!(*status, 529),
                other => panic!("want Provider{{529}}, got {other:?}"),
            }
        }

        #[test]
        fn unknown_error_kind_maps_to_provider_500() {
            let mut c = wire::StreamConverter::new("m");
            let e = ev(
                "error",
                r#"{"type":"error","error":{"type":"weird_one","message":"???"}}"#,
            );
            let out = c.process(&e);
            match &out[0] {
                Err(LlmError::Provider { status, .. }) => assert_eq!(*status, 500),
                other => panic!("want Provider{{500}}, got {other:?}"),
            }
        }

        #[test]
        fn stop_reason_max_tokens_maps_to_length() {
            let mut c = wire::StreamConverter::new("m");
            run(
                &mut c,
                [
                    &ev(
                        "message_delta",
                        r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":1}}"#,
                    ),
                    &ev("message_stop", r#"{"type":"message_stop"}"#),
                ],
            );
            // Build done event happens automatically; check via finish flag.
            // We need to inspect emitted events:
            let mut c2 = wire::StreamConverter::new("m");
            let out = run(
                &mut c2,
                [
                    &ev(
                        "message_delta",
                        r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":1}}"#,
                    ),
                    &ev("message_stop", r#"{"type":"message_stop"}"#),
                ],
            );
            let done = out.last().unwrap().as_ref().unwrap();
            match done {
                StreamEvent::Done { finish, .. } => {
                    assert!(matches!(finish, FinishReason::Length));
                }
                e => panic!("want Done, got {e:?}"),
            }
        }

        #[test]
        fn stop_reason_stop_sequence_maps_to_stop() {
            let mut c = wire::StreamConverter::new("m");
            let out = run(
                &mut c,
                [
                    &ev(
                        "message_delta",
                        r#"{"type":"message_delta","delta":{"stop_reason":"stop_sequence"}}"#,
                    ),
                    &ev("message_stop", r#"{}"#),
                ],
            );
            let done = out.last().unwrap().as_ref().unwrap();
            assert!(matches!(done, StreamEvent::Done { finish: FinishReason::Stop, .. }));
        }

        #[test]
        fn stop_reason_refusal_maps_to_refusal() {
            let mut c = wire::StreamConverter::new("m");
            let out = run(
                &mut c,
                [
                    &ev(
                        "message_delta",
                        r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#,
                    ),
                    &ev("message_stop", r#"{}"#),
                ],
            );
            let done = out.last().unwrap().as_ref().unwrap();
            assert!(matches!(done, StreamEvent::Done { finish: FinishReason::Refusal, .. }));
        }

        #[test]
        fn message_delta_overwrites_running_output_tokens() {
            let mut c = wire::StreamConverter::new("m");
            run(
                &mut c,
                [&ev(
                    "message_start",
                    r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":5,"output_tokens":1}}}"#,
                )],
            );
            assert_eq!(c.debug_usage().output_tokens, 1);
            run(
                &mut c,
                [&ev(
                    "message_delta",
                    r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}"#,
                )],
            );
            // Running total — overwrite, not accumulate.
            assert_eq!(c.debug_usage().output_tokens, 12);
        }

        #[test]
        fn missing_event_header_uses_payload_type() {
            // Some upstreams omit the SSE `event:` field; we should
            // fall back to the JSON `type` field.
            let mut c = wire::StreamConverter::new("m");
            let raw = SseEvent {
                event: String::new(),
                data: r#"{"type":"message_stop"}"#.to_string(),
            };
            let out = c.process(&raw);
            assert!(c.is_finished());
            assert_eq!(out.len(), 1);
            assert!(matches!(out[0].as_ref().unwrap(), StreamEvent::Done { .. }));
        }

        #[test]
        fn unknown_kind_is_silently_ignored() {
            let mut c = wire::StreamConverter::new("m");
            let raw = ev("xx_future_event", r#"{"type":"xx_future_event"}"#);
            let out = c.process(&raw);
            assert!(out.is_empty());
            assert!(!c.is_finished());
        }
    }

    // ---- AnthropicStream (bytes → StreamEvent) ---------------------------

    mod anthropic_stream {
        use super::*;
        use bytes::Bytes;
        use futures_util::stream;
        use futures_util::StreamExt;

        // Build an HTTP-like body: each canonical SSE event is two
        // lines (event:..\ndata:..) followed by an empty line.
        fn sse_body(events: &[(&str, &str)]) -> String {
            let mut s = String::new();
            for (name, data) in events {
                s.push_str(&format!("event: {name}\ndata: {data}\n\n"));
            }
            s
        }

        async fn collect(
            chunks: Vec<Bytes>,
        ) -> Vec<Result<StreamEvent>> {
            let bytes_stream = stream::iter(
                chunks.into_iter().map(Ok::<_, reqwest::Error>),
            );
            let mut s = wire::AnthropicStream::new(bytes_stream, "claude-x");
            let mut out = Vec::new();
            while let Some(ev) = s.next().await {
                out.push(ev);
            }
            out
        }

        #[tokio::test]
        async fn end_to_end_text_message_in_one_chunk() {
            let body = sse_body(&[
                (
                    "message_start",
                    r#"{"type":"message_start","message":{"model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":4,"output_tokens":0}}}"#,
                ),
                (
                    "content_block_start",
                    r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
                ),
                (
                    "content_block_delta",
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"OK"}}"#,
                ),
                (
                    "content_block_stop",
                    r#"{"type":"content_block_stop","index":0}"#,
                ),
                (
                    "message_delta",
                    r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
                ),
                ("message_stop", r#"{"type":"message_stop"}"#),
            ]);
            let chunks = vec![Bytes::from(body)];
            let out: Vec<StreamEvent> = collect(chunks)
                .await
                .into_iter()
                .map(|r| r.expect("ok"))
                .collect();

            assert_eq!(out.len(), 2, "got: {out:?}");
            assert!(matches!(out[0], StreamEvent::TextDelta { ref text } if text == "OK"));
            assert!(matches!(out[1], StreamEvent::Done { .. }));
        }

        #[tokio::test]
        async fn handles_byte_split_across_chunks() {
            // Same body, but chopped at every byte. Parser must
            // tolerate fine-grained chunking.
            let body = sse_body(&[
                (
                    "content_block_delta",
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
                ),
                ("message_stop", r#"{"type":"message_stop"}"#),
            ]);
            let chunks: Vec<Bytes> = body
                .as_bytes()
                .iter()
                .map(|b| Bytes::from(vec![*b]))
                .collect();
            let out: Vec<StreamEvent> = collect(chunks)
                .await
                .into_iter()
                .map(|r| r.expect("ok"))
                .collect();
            // TextDelta + Done.
            assert_eq!(out.len(), 2, "got: {out:?}");
            assert!(matches!(out[0], StreamEvent::TextDelta { ref text } if text == "hi"));
            assert!(matches!(out[1], StreamEvent::Done { .. }));
        }

        #[tokio::test]
        async fn unterminated_final_event_still_processed_on_eof() {
            // No trailing blank line — parser.finish() should flush.
            let body = "event: message_stop\ndata: {\"type\":\"message_stop\"}".to_string();
            let chunks = vec![Bytes::from(body)];
            let out: Vec<StreamEvent> = collect(chunks)
                .await
                .into_iter()
                .map(|r| r.expect("ok"))
                .collect();
            assert_eq!(out.len(), 1);
            assert!(matches!(out[0], StreamEvent::Done { .. }));
        }

        #[tokio::test]
        async fn ping_chunks_yield_no_events() {
            let body = sse_body(&[("ping", r#"{"type":"ping"}"#)]);
            let chunks = vec![Bytes::from(body)];
            let out = collect(chunks).await;
            assert!(out.is_empty(), "got: {out:?}");
        }

        #[tokio::test]
        async fn stream_terminates_at_message_stop_and_drops_trailing_garbage() {
            // After message_stop, converter sets finished=true so the
            // wrapper stops pulling. Garbage bytes after that should
            // not panic.
            let body = format!(
                "{}{}",
                sse_body(&[("message_stop", r#"{"type":"message_stop"}"#)]),
                "event: noise\ndata: garbage\n\n",
            );
            let chunks = vec![Bytes::from(body)];
            let out: Vec<StreamEvent> = collect(chunks)
                .await
                .into_iter()
                .map(|r| r.expect("ok"))
                .collect();
            assert_eq!(out.len(), 1);
            assert!(matches!(out[0], StreamEvent::Done { .. }));
        }
    }
}
