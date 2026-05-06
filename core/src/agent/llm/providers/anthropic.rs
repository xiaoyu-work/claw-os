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

#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub extra_headers: HashMap<String, String>,
    pub request_timeout: Duration,
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

        Self {
            base_url,
            api_key,
            model: model.to_string(),
            extra_headers: agent.extra_headers.clone(),
            request_timeout,
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
        self.cfg.api_key.is_some()
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = wire::build_request_body(&request, &self.cfg.model, false);

        let mut http = self
            .client
            .post(self.endpoint())
            .header("Content-Type", "application/json")
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body);

        if let Some(key) = &self.cfg.api_key {
            http = http.header("x-api-key", key);
        }
        for (k, v) in &self.cfg.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }

        let resp = http.send().await?;
        let status = resp.status();
        let retry_after_secs = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        let bytes = resp.bytes().await.map_err(LlmError::Transport)?;

        if !status.is_success() {
            return Err(wire::classify_http_error(status, &bytes, retry_after_secs));
        }

        let parsed: wire::Response =
            serde_json::from_slice(&bytes).map_err(|e| LlmError::Parse(e.to_string()))?;

        wire::response_to_chat(parsed, &self.cfg.model)
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
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

/// Wire-format adapters: serialise our internal types into Anthropic's
/// Messages schema and parse responses back. Kept private + pure (no IO)
/// so we can unit-test it without spinning up an HTTP server.
pub(crate) mod wire {
    use super::*;

    // --- Request --------------------------------------------------------

    /// Build the JSON body for `POST /v1/messages`. Pure — no IO.
    ///
    /// Key differences vs OpenAI:
    /// - `system` is hoisted out of `messages` to top level
    /// - `max_tokens` is always present (Anthropic requires it)
    /// - tool result blocks live inside a `user` message, not their own role
    pub(crate) fn build_request_body(
        request: &ChatRequest,
        model: &str,
        stream: bool,
    ) -> serde_json::Value {
        let messages: Vec<serde_json::Value> =
            request.messages.iter().map(message_to_json).collect();

        let tools: Vec<serde_json::Value> = request.tools.iter().map(tool_to_json).collect();

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
            "max_tokens": request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        });

        if let Some(obj) = body.as_object_mut() {
            if let Some(sys) = &request.system {
                if !sys.is_empty() {
                    obj.insert("system".into(), serde_json::json!(sys));
                }
            }
            if !tools.is_empty() {
                obj.insert("tools".into(), serde_json::Value::Array(tools));
                obj.insert("tool_choice".into(), tool_choice_to_json(&request.tool_choice));
            }
            if let Some(v) = request.temperature {
                obj.insert("temperature".into(), serde_json::json!(v));
            }
            if let Some(v) = request.top_p {
                obj.insert("top_p".into(), serde_json::json!(v));
            }
            if !request.stop_sequences.is_empty() {
                obj.insert(
                    "stop_sequences".into(),
                    serde_json::json!(request.stop_sequences),
                );
            }
            if stream {
                obj.insert("stream".into(), serde_json::json!(true));
            }
            if let serde_json::Value::Object(extra) = &request.extra {
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

    fn message_to_json(m: &crate::agent::llm::Message) -> serde_json::Value {
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

        let blocks: Vec<serde_json::Value> = m.content.iter().map(content_block_to_json).collect();

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

    fn tool_to_json(t: &Tool) -> serde_json::Value {
        serde_json::json!({
            "name": t.name,
            "description": t.description,
            "input_schema": t.input_schema,
        })
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
}
