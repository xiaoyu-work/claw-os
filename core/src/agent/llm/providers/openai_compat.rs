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
use futures_util::stream::{self, BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::agent::llm::{
    ChatRequest, ChatResponse, ContentBlock, FinishReason, LlmError, Provider, Result, Role,
    StreamEvent, Tool, ToolCall, ToolChoice, Usage,
};
use crate::config::AgentConfig;

pub const PROVIDER_NAME: &str = "openai";

/// Names this provider answers to in the registry. Adding an alias here
/// only changes the `name()` returned and the default base URL — the
/// wire format is identical.
pub const PROVIDER_ALIASES: &[&str] = &["openai", "xai", "deepseek", "openrouter", "ollama"];

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_XAI_BASE: &str = "https://api.x.ai/v1";
const DEFAULT_DEEPSEEK_BASE: &str = "https://api.deepseek.com/v1";
const DEFAULT_OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";
const DEFAULT_OLLAMA_BASE: &str = "http://localhost:11434/v1";

/// Resolve the default base URL for one of [`PROVIDER_ALIASES`]. Falls
/// back to OpenAI's URL if the alias is unknown.
pub fn default_base_url_for(alias: &str) -> &'static str {
    match alias {
        "xai" => DEFAULT_XAI_BASE,
        "deepseek" => DEFAULT_DEEPSEEK_BASE,
        "openrouter" => DEFAULT_OPENROUTER_BASE,
        "ollama" => DEFAULT_OLLAMA_BASE,
        _ => DEFAULT_OPENAI_BASE,
    }
}

/// Whether the alias's default base URL is local-only (no API key
/// required). Lets `is_configured()` return true for Ollama without a
/// stored key.
fn alias_is_local_default(alias: &str) -> bool {
    matches!(alias, "ollama")
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

impl OpenAICompatProvider {
    pub fn new(cfg: OpenAICompatConfig) -> Self {
        let mut builder =
            reqwest::Client::builder().user_agent(concat!("cos-agent/", env!("CARGO_PKG_VERSION")));
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
        // Split off any query string (Azure OpenAI requires
        // ?api-version=...). Append the path, then re-attach the
        // query.
        let (base, query) = match self.cfg.base_url.split_once('?') {
            Some((b, q)) => (b.trim_end_matches('/'), Some(q)),
            None => (self.cfg.base_url.as_str(), None),
        };
        match query {
            Some(q) => format!("{base}/chat/completions?{q}"),
            None => format!("{base}/chat/completions"),
        }
    }
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
        self.cfg.api_key.is_some()
            || self.cfg.pool.as_ref().is_some_and(|p| !p.is_empty())
            || alias_is_local_default(&self.cfg.alias)
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = wire::build_request_body(&request, &self.cfg.model, false);

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

        let bearer: Option<&str> = match &lease {
            Some(l) => Some(l.value()),
            None => self.cfg.api_key.as_deref(),
        };

        let mut http = self
            .client
            .post(self.endpoint())
            .header("Content-Type", "application/json")
            .json(&body);

        if let Some(key) = bearer {
            http = http.bearer_auth(key);
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
            let err = wire::classify_http_error(status, &bytes);
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
                // Body parse error after a 2xx — treat as a caller-side
                // problem (we asked for something the upstream
                // returned in a shape we don't understand). Don't
                // blame the key.
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
        // Phase 1: ship a non-SSE shim — call chat() then emit
        // Message + Done. Real SSE delta streaming lands in Phase 5
        // (alongside prompt caching) once a use case demands it.
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
            messages.push(message_to_json(m));
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
        let role = role_to_str(m.role);

        // Tool result: each ToolResult block becomes its own message with
        // `role=tool` + tool_call_id. OpenAI's schema requires one tool
        // message per tool call. We collapse to a single message here when
        // the input has only one ToolResult; otherwise the caller must
        // pre-split (the runtime already does).
        if let Some(ContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        }) = m.content.first()
        {
            if m.content.len() == 1 {
                return serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": content,
                });
            }
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
                    // Already handled above for the single-block case.
                }
                ContentBlock::Image { media_type, data } => {
                    // OpenAI vision: send as content list with image_url.
                    text_parts.push(format!("[image {} base64 attached]", media_type));
                    let _ = data; // future: emit as { type: image_url } block
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
        serde_json::Value::Object(obj)
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
            let parsed: serde_json::Value =
                serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Null);
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
                return msg.to_string();
            }
            if let Some(msg) = v.get("message").and_then(|m| m.as_str()) {
                return msg.to_string();
            }
        }
        body.chars().take(500).collect()
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
        assert!(default_base_url_for("__unknown__").starts_with("https://api.openai.com"));
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
        // Azure OpenAI requires ?api-version=...
        let mut c = cfg();
        c.base_url = Some(
            "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/gpt-5.4-mini?api-version=2024-12-01-preview".into(),
        );
        let provider = OpenAICompatProvider::from_agent_config("openai", "gpt-5.4-mini", &c);
        assert_eq!(
            provider.endpoint(),
            "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/gpt-5.4-mini/chat/completions?api-version=2024-12-01-preview"
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
    fn body_merges_extras() {
        let mut r = req_text("hi");
        r.extra = serde_json::json!({"seed": 42, "response_format": {"type":"json_object"}});
        let body = wire::build_request_body(&r, "m", false);
        assert_eq!(body["seed"], 42);
        assert_eq!(body["response_format"]["type"], "json_object");
    }

    // ---- response parsing ------------------------------------------------

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
}
