//! Google Gemini (AI Studio) provider.
//!
//! Targets the public `generativelanguage.googleapis.com` REST surface
//! (a.k.a. AI Studio / Generative Language API). Vertex AI and the
//! Gemini Cloud Code OAuth flow are intentionally out of scope here —
//! the plan calls those out as separate providers (Q3 lists `gemini`
//! alongside `bedrock`; Vertex/CodeAssist would be siblings, not folded
//! into this one).
//!
//! Wire shape essentials (very different from OpenAI / Anthropic):
//! - Endpoint: `POST {base}/v1beta/models/{model}:generateContent?key={api_key}`
//! - Auth is a `?key=` query parameter (or `x-goog-api-key` header). We
//!   use the header form so the credential never lands in URL strings
//!   (logs, tracing, redirects).
//! - Roles are `user` and `model` (not `assistant`).
//! - System prompt is hoisted to top-level `systemInstruction`.
//! - Generation knobs (`maxOutputTokens`, `temperature`, ...) live under
//!   `generationConfig`, not the body root.
//! - Tools are nested: `tools: [{ functionDeclarations: [...] }]`.
//! - A function call is a content part — `{functionCall: {name, args}}` —
//!   inside a `model` turn. There is **no upstream call ID** — Gemini
//!   matches function responses by `name`. We synthesize a synthetic
//!   `id = "<name>::<seq>"` so the runtime's per-call tracking still
//!   works, then strip the suffix when serialising the response part.
//! - Tool result is `{functionResponse: {name, response}}` inside a
//!   `user` turn.
//! - `finishReason` enum is upper-snake-case: `STOP`, `MAX_TOKENS`,
//!   `SAFETY`, `RECITATION`, `OTHER`.
//! - Errors: `{error: {code, message, status}}`. Rate limits surface as
//!   HTTP 429 with `Retry-After` header.

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
use crate::config::AgentConfig;

pub const PROVIDER_NAME: &str = "gemini";

const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com";

/// API version used in the URL path (Google promotes v1beta as the
/// long-lived feature track).
pub const API_VERSION: &str = "v1beta";

/// Fallback `maxOutputTokens` when caller didn't specify. Gemini will
/// happily generate without it (the cap is server-side default), but
/// pinning a sensible default keeps cost predictable.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Marker we splice into our internal call IDs so we can recover the
/// function name later. `<name>::<seq>` round-trips losslessly through
/// the runtime even though Gemini itself doesn't track call IDs.
const ID_SEP: &str = "::";

pub fn default_base_url() -> &'static str {
    DEFAULT_BASE
}

#[derive(Clone)]
pub struct GeminiConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub extra_headers: HashMap<String, String>,
    pub request_timeout: Duration,
    /// Optional multi-key credential pool. See
    /// `OpenAICompatConfig::pool` for semantics.
    pub pool: Option<Arc<crate::agent::llm::credential_pool::Pool>>,
}

impl std::fmt::Debug for GeminiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiConfig")
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

impl GeminiConfig {
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
            "provider:gemini",
            agent,
        ) {
            Ok(Some(p)) => Some(Arc::new(p)),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(
                    target: "cos::agent::llm::pool",
                    "credential pool for provider 'gemini' declared but unresolved: {e}; \
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

pub struct GeminiProvider {
    cfg: GeminiConfig,
    client: reqwest::Client,
}

impl GeminiProvider {
    pub fn new(cfg: GeminiConfig) -> Self {
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("cos-agent/", env!("CARGO_PKG_VERSION")))
            // MEDIUM-14: cap the TCP/TLS handshake separately from
            // the overall request budget so a black-holed DNS or
            // firewalled host can't tie up the kernel.
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(60));
        if cfg.request_timeout > Duration::from_secs(0) {
            builder = builder.timeout(cfg.request_timeout);
        }
        let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        Self { cfg, client }
    }

    pub fn from_agent_config(model: &str, agent: &AgentConfig) -> Self {
        Self::new(GeminiConfig::from_agent_config(model, agent))
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/{}/models/{}:generateContent",
            self.cfg.base_url, API_VERSION, self.cfg.model
        )
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn supported_models(&self) -> Vec<String> {
        vec![self.cfg.model.clone()]
    }

    fn is_configured(&self) -> bool {
        self.cfg.api_key.is_some() || self.cfg.pool.as_ref().is_some_and(|p| !p.is_empty())
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let body = wire::build_request_body(&request, false);

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
            .json(&body);

        if let Some(key) = api_key {
            // Header form — keeps the key out of URL strings (logs,
            // redirects, tracing).
            http = http.header("x-goog-api-key", key);
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
        // HIGH-5: bound the response body so a hostile upstream
        // can't OOM the kernel.
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
                // MEDIUM-12: a 2xx with un-parseable body is the
                // upstream's bug, not ours. Mark the lease as
                // Transient (don't permanently penalise the key)
                // and surface as UpstreamMalformed so callers can
                // distinguish from caller-side bugs.
                if let (Some(pool), Some(l)) = (&self.cfg.pool, &lease) {
                    pool.report_failure(
                        l,
                        crate::agent::llm::credential_pool::FailureClass::Transient,
                    );
                }
                return Err(LlmError::UpstreamMalformed(format!(
                    "gemini response: {e}"
                )));
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
        // HIGH-4: real SSE delta streaming requires speaking
        // Gemini's `:streamGenerateContent?alt=sse` endpoint, which
        // differs enough from generateContent that wiring it is a
        // larger refactor (incremental candidate parts + finish
        // events). Until that lands, we route through `chat()` so
        // callers at least get the (now body-capped) full response;
        // the shim still surfaces TextDelta + Done so downstream
        // streaming consumers don't break.
        //
        // TODO: implement an OpenAi-style streaming converter for
        // Gemini's SSE shape (see openai_compat::wire::OpenAiStream
        // for the pattern). Tracked in the LLM hardening backlog.
        let response = self.chat(request).await?;
        let finish = response.finish_reason;
        let usage = response.usage.clone();
        let mut events: Vec<std::result::Result<StreamEvent, LlmError>> = Vec::new();
        // Surface any text content as a single TextDelta so the
        // caller's SSE consumer sees the same event shape as a real
        // streaming provider.
        for block in &response.content {
            if let ContentBlock::Text { text } = block {
                if !text.is_empty() {
                    events.push(Ok(StreamEvent::TextDelta {
                        text: text.clone(),
                    }));
                }
            }
        }
        events.push(Ok(StreamEvent::Message(response)));
        events.push(Ok(StreamEvent::Done { finish, usage }));
        Ok(futures_util::stream::iter(events).boxed())
    }
}

/// Wire-format adapters: serialise into Gemini's
/// generateContent / generateContent shape and parse responses.
pub(crate) mod wire {
    use super::*;

    // --- Request --------------------------------------------------------

    pub(crate) fn build_request_body(request: &ChatRequest, stream: bool) -> serde_json::Value {
        let contents: Vec<serde_json::Value> =
            request.messages.iter().map(message_to_json).collect();

        let mut body = serde_json::json!({
            "contents": contents,
        });

        if let Some(obj) = body.as_object_mut() {
            // System hoisted out of messages.
            if let Some(sys) = &request.system {
                if !sys.is_empty() {
                    obj.insert(
                        "systemInstruction".into(),
                        serde_json::json!({"parts": [{"text": sys}]}),
                    );
                }
            }

            // Generation knobs go under generationConfig, not body root.
            let mut gen_cfg = serde_json::Map::new();
            gen_cfg.insert(
                "maxOutputTokens".into(),
                serde_json::json!(request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
            );
            if let Some(v) = request.temperature {
                gen_cfg.insert("temperature".into(), serde_json::json!(v));
            }
            if let Some(v) = request.top_p {
                gen_cfg.insert("topP".into(), serde_json::json!(v));
            }
            if !request.stop_sequences.is_empty() {
                gen_cfg.insert(
                    "stopSequences".into(),
                    serde_json::json!(request.stop_sequences),
                );
            }
            obj.insert(
                "generationConfig".into(),
                serde_json::Value::Object(gen_cfg),
            );

            // Tools — nested as functionDeclarations.
            if !request.tools.is_empty() {
                let decls: Vec<serde_json::Value> =
                    request.tools.iter().map(tool_to_json).collect();
                obj.insert(
                    "tools".into(),
                    serde_json::json!([{"functionDeclarations": decls}]),
                );
                obj.insert(
                    "toolConfig".into(),
                    tool_choice_to_json(&request.tool_choice),
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

    /// Gemini's role vocabulary is `user` and `model`. System content is
    /// hoisted away. Tool results live under `user`.
    fn role_to_str(role: Role) -> &'static str {
        match role {
            Role::Assistant => "model",
            Role::System | Role::User | Role::Tool => "user",
        }
    }

    fn message_to_json(m: &crate::agent::llm::Message) -> serde_json::Value {
        let role = role_to_str(m.role);
        let parts: Vec<serde_json::Value> = m.content.iter().map(content_block_to_part).collect();
        serde_json::json!({
            "role": role,
            "parts": parts,
        })
    }

    fn content_block_to_part(b: &ContentBlock) -> serde_json::Value {
        match b {
            ContentBlock::Text { text } => serde_json::json!({"text": text}),
            ContentBlock::ToolUse { name, input, .. } => serde_json::json!({
                "functionCall": {
                    "name": name,
                    "args": input,
                }
            }),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                // Recover the function name from our synthetic id
                // ("<name>::<seq>"). If a caller hands us a non-synthetic
                // id (no separator), treat the whole thing as the name.
                let name = strip_id_seq(tool_use_id);
                // Gemini's `response` is an arbitrary JSON object; if our
                // tool result content is valid JSON, pass it through;
                // otherwise wrap it as `{"content": "..."}`.
                let response_value = serde_json::from_str::<serde_json::Value>(content)
                    .unwrap_or_else(|_| serde_json::json!({"content": content}));
                serde_json::json!({
                    "functionResponse": {
                        "name": name,
                        "response": response_value,
                    }
                })
            }
            ContentBlock::Image { media_type, data } => serde_json::json!({
                "inlineData": {
                    "mimeType": media_type,
                    "data": data,
                }
            }),
        }
    }

    /// Strip our `<name>::<seq>` synthetic id back down to `<name>`.
    /// Idempotent — IDs without the separator pass through unchanged.
    pub(crate) fn strip_id_seq(id: &str) -> &str {
        match id.split_once(ID_SEP) {
            Some((name, _)) => name,
            None => id,
        }
    }

    fn tool_to_json(t: &Tool) -> serde_json::Value {
        serde_json::json!({
            "name": t.name,
            "description": t.description,
            "parameters": t.input_schema,
        })
    }

    fn tool_choice_to_json(c: &ToolChoice) -> serde_json::Value {
        let mode = match c {
            ToolChoice::Auto => "AUTO",
            ToolChoice::None => "NONE",
            ToolChoice::Required => "ANY",
            ToolChoice::Tool { .. } => "ANY",
        };
        let mut cfg = serde_json::json!({"mode": mode});
        if let ToolChoice::Tool { name } = c {
            if let Some(o) = cfg.as_object_mut() {
                o.insert("allowedFunctionNames".into(), serde_json::json!([name]));
            }
        }
        serde_json::json!({"functionCallingConfig": cfg})
    }

    // --- Response -------------------------------------------------------

    #[derive(Debug, Deserialize)]
    pub(crate) struct Response {
        #[serde(default)]
        pub candidates: Vec<Candidate>,
        #[serde(default, rename = "usageMetadata")]
        pub usage_metadata: Option<UsageJson>,
        #[serde(default, rename = "modelVersion")]
        pub model_version: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct Candidate {
        #[serde(default)]
        pub content: Option<CandidateContent>,
        #[serde(default, rename = "finishReason")]
        pub finish_reason: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct CandidateContent {
        #[serde(default)]
        pub parts: Vec<Part>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(untagged)]
    pub(crate) enum Part {
        Text {
            text: String,
        },
        FunctionCall {
            #[serde(rename = "functionCall")]
            function_call: FunctionCall,
        },
        // Forward-compat: thinking, executableCode, codeExecutionResult,
        // inlineData(*from model*) — surface a sentinel so `untagged`
        // doesn't blow up on novel parts. Empty struct ≅ "ignore me".
        Other(serde_json::Value),
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct FunctionCall {
        pub name: String,
        #[serde(default)]
        pub args: serde_json::Value,
    }

    #[derive(Debug, Default, Deserialize, Serialize)]
    pub(crate) struct UsageJson {
        #[serde(default, rename = "promptTokenCount")]
        pub prompt_token_count: u32,
        #[serde(default, rename = "candidatesTokenCount")]
        pub candidates_token_count: u32,
        #[serde(default, rename = "cachedContentTokenCount")]
        pub cached_content_token_count: u32,
    }

    pub(crate) fn response_to_chat(resp: Response, fallback_model: &str) -> Result<ChatResponse> {
        let candidate = resp
            .candidates
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::Parse("response had no candidates".into()))?;

        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut fc_seq: u32 = 0;

        if let Some(content) = candidate.content {
            for part in content.parts {
                match part {
                    Part::Text { text } => {
                        if !text.is_empty() {
                            content_blocks.push(ContentBlock::Text { text });
                        }
                    }
                    Part::FunctionCall { function_call } => {
                        let id = format!("{}{}{}", function_call.name, ID_SEP, fc_seq);
                        fc_seq += 1;
                        content_blocks.push(ContentBlock::ToolUse {
                            id: id.clone(),
                            name: function_call.name.clone(),
                            input: function_call.args.clone(),
                        });
                        tool_calls.push(ToolCall {
                            id,
                            name: function_call.name,
                            input: function_call.args,
                        });
                    }
                    Part::Other(_) => {
                        // Skip unknown parts — forward-compat.
                    }
                }
            }
        }

        let finish_reason = match candidate.finish_reason.as_deref() {
            Some("STOP") | Some("FINISH_REASON_UNSPECIFIED") | None => FinishReason::Stop,
            Some("MAX_TOKENS") => FinishReason::Length,
            Some("SAFETY")
            | Some("RECITATION")
            | Some("BLOCKLIST")
            | Some("PROHIBITED_CONTENT")
            | Some("SPII") => FinishReason::ContentFilter,
            // Gemini doesn't emit a "tool_use" finish reason — it just
            // includes a functionCall part with STOP. Detect tool_use by
            // checking whether tool_calls accumulated.
            Some(_) => FinishReason::Other,
        };

        // If the candidate stopped naturally but emitted tool calls,
        // surface that as ToolUse finish so the runtime knows to dispatch.
        let finish_reason = if !tool_calls.is_empty()
            && matches!(finish_reason, FinishReason::Stop | FinishReason::Other)
        {
            FinishReason::ToolUse
        } else {
            finish_reason
        };

        let usage = resp
            .usage_metadata
            .map(|u| Usage {
                input_tokens: u.prompt_token_count,
                output_tokens: u.candidates_token_count,
                cache_read_tokens: u.cached_content_token_count,
                cache_write_tokens: 0,
            })
            .unwrap_or_default();

        Ok(ChatResponse {
            model: resp
                .model_version
                .unwrap_or_else(|| fallback_model.to_string()),
            content: content_blocks,
            tool_calls,
            finish_reason,
            usage,
        })
    }

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
            _ => LlmError::Provider {
                status: status.as_u16(),
                message: upstream_message,
            },
        }
    }

    fn extract_error_message(body: &str) -> String {
        // Google: `{"error":{"code":N,"message":"...","status":"..."}}`
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
            if let Some(msg) = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
            {
                return crate::agent::llm::redact_body_for_error(msg);
            }
        }
        // SECURITY: error bodies frequently echo prompts + key
        // fragments. Run through the bearer / API-key masking
        // helper before surfacing.
        crate::agent::llm::redact_body_for_error(body)
    }
}

pub fn is_alias(name: &str) -> bool {
    name == PROVIDER_NAME
}

pub fn build_provider(model: &str, agent: &AgentConfig) -> Arc<dyn Provider> {
    Arc::new(GeminiProvider::from_agent_config(model, agent))
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
            model: "gemini-2.0-flash".into(),
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
    fn default_base_url_is_googleapis_com() {
        assert!(default_base_url().contains("generativelanguage.googleapis.com"));
    }

    #[test]
    fn config_uses_override_when_set() {
        let mut c = cfg();
        c.base_url = Some("https://my.proxy".into());
        let gc = GeminiConfig::from_agent_config("gemini-2.0-flash", &c);
        assert_eq!(gc.base_url, "https://my.proxy");
    }

    #[test]
    fn endpoint_includes_model_and_method() {
        let mut c = cfg();
        c.base_url = Some(DEFAULT_BASE.to_string());
        let provider = GeminiProvider::from_agent_config("gemini-1.5-pro", &c);
        assert_eq!(
            provider.endpoint(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-1.5-pro:generateContent"
        );
    }

    #[test]
    fn endpoint_strips_trailing_slash_in_base() {
        let mut c = cfg();
        c.base_url = Some(format!("{DEFAULT_BASE}/"));
        let provider = GeminiProvider::from_agent_config("gemini-2.0-flash", &c);
        assert!(provider.endpoint().starts_with("https://"));
        assert!(!provider.endpoint().contains("//v1beta"));
    }

    // ---- is_configured ---------------------------------------------------

    #[test]
    fn is_configured_true_when_api_key_present() {
        let mut c = cfg();
        c.api_key_env = Some("COS_TEST_GEMINI_KEY_X".into());
        std::env::set_var("COS_TEST_GEMINI_KEY_X", "AIza-x");
        let p = GeminiProvider::from_agent_config("gemini-2.0-flash", &c);
        assert!(p.is_configured());
        std::env::remove_var("COS_TEST_GEMINI_KEY_X");
    }

    #[test]
    fn is_configured_false_without_key() {
        let p = GeminiProvider::from_agent_config("gemini-2.0-flash", &cfg());
        assert!(!p.is_configured());
    }

    // ---- request body serialisation --------------------------------------

    #[test]
    fn builds_minimal_chat_body() {
        let r = req_text("hello");
        let body = wire::build_request_body(&r, false);
        // System hoisted to systemInstruction at top level.
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "you are helpful"
        );
        // Contents has only the user turn (system is NOT a message).
        assert_eq!(body["contents"].as_array().unwrap().len(), 1);
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "hello");
        // Generation knobs nested under generationConfig.
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 64);
        assert_eq!(body["generationConfig"]["temperature"], 0.5);
        // No tools section.
        assert!(body.get("tools").is_none());
        // No model field at top level (model is in URL).
        assert!(body.get("model").is_none());
    }

    #[test]
    fn always_emits_max_output_tokens_under_generation_config() {
        let mut r = req_text("hi");
        r.max_tokens = None;
        let body = wire::build_request_body(&r, false);
        assert_eq!(
            body["generationConfig"]["maxOutputTokens"],
            DEFAULT_MAX_TOKENS
        );
    }

    #[test]
    fn body_omits_system_instruction_when_empty() {
        let mut r = req_text("hi");
        r.system = Some(String::new());
        let body = wire::build_request_body(&r, false);
        assert!(body.get("systemInstruction").is_none());
    }

    #[test]
    fn body_omits_system_instruction_when_none() {
        let mut r = req_text("hi");
        r.system = None;
        let body = wire::build_request_body(&r, false);
        assert!(body.get("systemInstruction").is_none());
    }

    #[test]
    fn body_uses_googleapis_role_names() {
        let mut r = req_text("ignored");
        r.messages.push(crate::agent::llm::Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "previous".into(),
            }],
        });
        let body = wire::build_request_body(&r, false);
        // user → user; Assistant → "model" (NOT "assistant").
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][1]["role"], "model");
    }

    #[test]
    fn body_includes_tools_under_function_declarations() {
        let mut r = req_text("call tool");
        r.tools = vec![Tool {
            name: "echo".into(),
            description: "echo it".into(),
            input_schema: serde_json::json!({"type":"object","properties":{}}),
        }];
        let body = wire::build_request_body(&r, false);
        // tools is a SINGLE-element array of {functionDeclarations: [...]}
        assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "echo");
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["description"],
            "echo it"
        );
        // toolConfig.functionCallingConfig.mode = AUTO
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
    }

    #[test]
    fn body_renders_assistant_function_call_part() {
        let mut r = req_text("ignored");
        r.messages.push(crate::agent::llm::Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "echo::0".into(),
                name: "echo".into(),
                input: serde_json::json!({"text": "hi"}),
            }],
        });
        let body = wire::build_request_body(&r, false);
        let asst = &body["contents"][1];
        assert_eq!(asst["role"], "model");
        assert_eq!(asst["parts"][0]["functionCall"]["name"], "echo");
        assert_eq!(asst["parts"][0]["functionCall"]["args"]["text"], "hi");
    }

    #[test]
    fn body_renders_tool_result_as_function_response_part() {
        let mut r = req_text("ignored");
        r.messages.push(crate::agent::llm::Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "echo::0".into(),
                is_error: false,
                content: "{\"ok\":true}".into(),
            }],
        });
        let body = wire::build_request_body(&r, false);
        let msg = &body["contents"][1];
        // Tool result lives under user role.
        assert_eq!(msg["role"], "user");
        assert_eq!(msg["parts"][0]["functionResponse"]["name"], "echo");
        // JSON content is parsed and inlined into `response`.
        assert_eq!(msg["parts"][0]["functionResponse"]["response"]["ok"], true);
    }

    #[test]
    fn body_wraps_non_json_tool_result_under_content_key() {
        let mut r = req_text("ignored");
        r.messages.push(crate::agent::llm::Message {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "echo::0".into(),
                is_error: false,
                content: "plain text result".into(),
            }],
        });
        let body = wire::build_request_body(&r, false);
        // Non-JSON content gets wrapped as {"content": "..."}.
        assert_eq!(
            body["contents"][1]["parts"][0]["functionResponse"]["response"]["content"],
            "plain text result"
        );
    }

    #[test]
    fn body_strips_id_seq_for_function_response_name() {
        // Multiple synthetic ids that all map back to "echo".
        for id in &["echo::0", "echo::42", "echo"] {
            let s = wire::strip_id_seq(id);
            assert_eq!(s, "echo", "strip_id_seq({id}) should yield 'echo'");
        }
    }

    #[test]
    fn body_emits_stop_sequences_under_generation_config() {
        let mut r = req_text("hi");
        r.stop_sequences = vec!["END".into()];
        let body = wire::build_request_body(&r, false);
        assert_eq!(body["generationConfig"]["stopSequences"][0], "END");
        // Should NOT appear at body root or under Anthropic's name.
        assert!(body.get("stop_sequences").is_none());
        assert!(body.get("stop").is_none());
    }

    #[test]
    fn body_renders_image_inline_data() {
        let mut r = req_text("ignored");
        r.messages.push(crate::agent::llm::Message {
            role: Role::User,
            content: vec![ContentBlock::Image {
                media_type: "image/jpeg".into(),
                data: "abc=".into(),
            }],
        });
        let body = wire::build_request_body(&r, false);
        let part = &body["contents"][1]["parts"][0];
        assert_eq!(part["inlineData"]["mimeType"], "image/jpeg");
        assert_eq!(part["inlineData"]["data"], "abc=");
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
        let body = wire::build_request_body(&r, false);
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
    }

    #[test]
    fn tool_choice_specific_tool_emits_allowed_function_names() {
        let mut r = req_text("call");
        r.tools = vec![Tool {
            name: "echo".into(),
            description: "".into(),
            input_schema: serde_json::json!({}),
        }];
        r.tool_choice = ToolChoice::Tool {
            name: "echo".into(),
        };
        let body = wire::build_request_body(&r, false);
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
        assert_eq!(
            body["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"][0],
            "echo"
        );
    }

    #[test]
    fn body_merges_extras() {
        let mut r = req_text("hi");
        r.extra = serde_json::json!({"safetySettings": [{"category": "HARM_CATEGORY_HARASSMENT", "threshold": "BLOCK_NONE"}]});
        let body = wire::build_request_body(&r, false);
        assert_eq!(
            body["safetySettings"][0]["category"],
            "HARM_CATEGORY_HARASSMENT"
        );
    }

    // ---- response parsing ------------------------------------------------

    #[test]
    fn parses_simple_text_response() {
        let raw = serde_json::json!({
            "candidates": [
                {
                    "content": {"role": "model", "parts": [{"text": "hello there"}]},
                    "finishReason": "STOP"
                }
            ],
            "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 3},
            "modelVersion": "gemini-2.0-flash-001"
        });
        let resp: wire::Response = serde_json::from_value(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        assert_eq!(chat.model, "gemini-2.0-flash-001");
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
    fn parses_function_call_response_synthesizes_id() {
        let raw = serde_json::json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [
                        {"functionCall": {"name": "lookup", "args": {"q": "weather"}}}
                    ]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 20, "candidatesTokenCount": 5}
        });
        let resp: wire::Response = serde_json::from_value(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        assert_eq!(chat.tool_calls.len(), 1);
        // Id format: "<name>::<seq>"
        assert_eq!(chat.tool_calls[0].id, "lookup::0");
        assert_eq!(chat.tool_calls[0].name, "lookup");
        assert_eq!(chat.tool_calls[0].input["q"], "weather");
        // Even though Gemini said STOP, we upgrade to ToolUse because a
        // function call was emitted.
        assert!(matches!(chat.finish_reason, FinishReason::ToolUse));
    }

    #[test]
    fn parses_parallel_function_calls_with_distinct_ids() {
        let raw = serde_json::json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [
                        {"functionCall": {"name": "echo", "args": {"x": 1}}},
                        {"functionCall": {"name": "echo", "args": {"x": 2}}}
                    ]
                },
                "finishReason": "STOP"
            }]
        });
        let resp: wire::Response = serde_json::from_value(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        assert_eq!(chat.tool_calls.len(), 2);
        // Two distinct synthetic ids: echo::0, echo::1
        assert_eq!(chat.tool_calls[0].id, "echo::0");
        assert_eq!(chat.tool_calls[1].id, "echo::1");
    }

    #[test]
    fn parses_response_with_unknown_part_type() {
        let raw = serde_json::json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [
                        {"thought": true, "text": "let me think..."},
                        {"text": "the answer"}
                    ]
                },
                "finishReason": "STOP"
            }]
        });
        let resp: wire::Response = serde_json::from_value(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        // Both text parts get extracted (the "thought:true" version still
        // has a `text` field — `untagged` enum picks Text).
        // The "the answer" must be present.
        let texts: Vec<&str> = chat
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            texts.iter().any(|t| t == &"the answer"),
            "must include the final text, got {texts:?}"
        );
    }

    #[test]
    fn finish_reason_max_tokens_maps_to_length() {
        let raw = serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "..."}]},
                "finishReason": "MAX_TOKENS"
            }]
        });
        let resp: wire::Response = serde_json::from_value(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        assert!(matches!(chat.finish_reason, FinishReason::Length));
    }

    #[test]
    fn finish_reason_safety_maps_to_content_filter() {
        let raw = serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": ""}]},
                "finishReason": "SAFETY"
            }]
        });
        let resp: wire::Response = serde_json::from_value(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        assert!(matches!(chat.finish_reason, FinishReason::ContentFilter));
    }

    #[test]
    fn parses_cached_content_token_count() {
        let raw = serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "ok"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 5,
                "candidatesTokenCount": 3,
                "cachedContentTokenCount": 2048
            }
        });
        let resp: wire::Response = serde_json::from_value(raw).unwrap();
        let chat = wire::response_to_chat(resp, "fallback").unwrap();
        assert_eq!(chat.usage.cache_read_tokens, 2048);
    }

    #[test]
    fn empty_candidates_array_is_parse_error() {
        let raw = serde_json::json!({"candidates": []});
        let resp: wire::Response = serde_json::from_value(raw).unwrap();
        let err = wire::response_to_chat(resp, "fallback").unwrap_err();
        match err {
            LlmError::Parse(msg) => assert!(msg.contains("no candidates")),
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    // ---- error classification --------------------------------------------

    #[test]
    fn classify_401_is_auth() {
        let err = wire::classify_http_error(
            reqwest::StatusCode::UNAUTHORIZED,
            br#"{"error":{"code":401,"message":"bad key","status":"UNAUTHENTICATED"}}"#,
            None,
        );
        assert!(matches!(err, LlmError::Auth));
    }

    #[test]
    fn classify_403_is_auth() {
        let err = wire::classify_http_error(
            reqwest::StatusCode::FORBIDDEN,
            br#"{"error":{"code":403,"message":"forbidden","status":"PERMISSION_DENIED"}}"#,
            None,
        );
        assert!(matches!(err, LlmError::Auth));
    }

    #[test]
    fn classify_429_uses_retry_after_header() {
        let err = wire::classify_http_error(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            br#"{"error":{"code":429,"message":"slow","status":"RESOURCE_EXHAUSTED"}}"#,
            Some(30),
        );
        match err {
            LlmError::RateLimited { retry_after_ms } => assert_eq!(retry_after_ms, 30_000),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn classify_500_extracts_message() {
        let err = wire::classify_http_error(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            br#"{"error":{"code":500,"message":"upstream borked","status":"INTERNAL"}}"#,
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
        assert!(is_alias("gemini"));
        assert!(!is_alias("anthropic"));
        assert!(!is_alias(""));
    }

    // ---- credential pool wiring ------------------------------------------

    #[test]
    fn gemini_no_pool_when_neither_plural_field_set() {
        let c = AgentConfig::default();
        let gc = GeminiConfig::from_agent_config("gemini-1.5-flash", &c);
        assert!(gc.pool.is_none());
    }

    #[test]
    fn gemini_pool_built_from_envs() {
        std::env::set_var("COS_TEST_GEM_POOL_A", "gem-aaa");
        std::env::set_var("COS_TEST_GEM_POOL_B", "gem-bbb");
        let mut c = AgentConfig::default();
        c.api_key_envs = vec!["COS_TEST_GEM_POOL_A".into(), "COS_TEST_GEM_POOL_B".into()];
        let gc = GeminiConfig::from_agent_config("gemini-1.5-flash", &c);
        std::env::remove_var("COS_TEST_GEM_POOL_A");
        std::env::remove_var("COS_TEST_GEM_POOL_B");
        let pool = gc.pool.expect("pool should be built");
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn gemini_is_configured_true_with_pool_only() {
        std::env::set_var("COS_TEST_GEM_POOL_ICONFIG", "gem-x");
        let mut c = AgentConfig::default();
        c.api_key_envs = vec!["COS_TEST_GEM_POOL_ICONFIG".into()];
        let gc = GeminiConfig::from_agent_config("gemini-1.5-flash", &c);
        std::env::remove_var("COS_TEST_GEM_POOL_ICONFIG");
        let provider = GeminiProvider::new(gc);
        assert!(provider.is_configured());
    }
}
