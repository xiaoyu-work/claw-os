use super::*;
use crate::agent::llm::accumulate::{accumulate_stream, NullSink};
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
fn body_filters_reserved_extras_and_preserves_provider_extras() {
    let mut r = req_text("hi");
    r.extra = serde_json::json!({
        "safetySettings": [{
            "category": "HARM_CATEGORY_HARASSMENT",
            "threshold": "BLOCK_NONE"
        }],
        "_cos_initiator": "agent",
        "_cos_trace": "internal",
        "__cache_system": true,
        "__private": true
    });
    let body = wire::build_request_body(&r, false);
    assert_eq!(
        body["safetySettings"][0]["category"],
        "HARM_CATEGORY_HARASSMENT"
    );
    for key in [
        "_cos_initiator",
        "_cos_trace",
        "__cache_system",
        "__private",
    ] {
        assert!(body.get(key).is_none(), "reserved extra leaked: {key}");
    }
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

#[tokio::test]
async fn buffered_stream_emits_complete_message_without_text_delta() {
    let call = ToolCall {
        id: "lookup::0".into(),
        name: "lookup".into(),
        input: serde_json::json!({"q": "weather"}),
    };
    let response = ChatResponse {
        model: "gemini-2.0-flash-001".into(),
        content: vec![
            ContentBlock::Text {
                text: "weather report".into(),
            },
            ContentBlock::ToolUse {
                id: call.id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
            },
        ],
        tool_calls: vec![call],
        finish_reason: FinishReason::ToolUse,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 3,
            cache_read_tokens: 2,
            cache_write_tokens: 0,
        },
    };

    let events = buffered_response_stream(response.clone())
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()
        .unwrap();

    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, StreamEvent::TextDelta { .. })),
        "buffered text must be represented only by Message"
    );
    match &events[0] {
        StreamEvent::Message(message) => {
            assert_eq!(message.model, "gemini-2.0-flash-001");
            assert_eq!(message.content.len(), 2);
            assert_eq!(message.tool_calls.len(), 1);
            assert_eq!(message.tool_calls[0].name, "lookup");
            assert!(matches!(message.finish_reason, FinishReason::ToolUse));
            assert_eq!(message.usage.input_tokens, 10);
            assert_eq!(message.usage.output_tokens, 3);
            assert_eq!(message.usage.cache_read_tokens, 2);
        }
        other => panic!("expected complete Message, got {other:?}"),
    }
    match &events[1] {
        StreamEvent::Done { finish, usage } => {
            assert!(matches!(finish, FinishReason::ToolUse));
            assert_eq!(usage.input_tokens, 10);
            assert_eq!(usage.output_tokens, 3);
            assert_eq!(usage.cache_read_tokens, 2);
        }
        other => panic!("expected Done, got {other:?}"),
    }

    let accumulated = accumulate_stream(
        buffered_response_stream(response),
        Arc::new(NullSink),
        "fallback",
    )
    .await
    .unwrap();
    assert_eq!(accumulated.model, "gemini-2.0-flash-001");
    assert_eq!(accumulated.content.len(), 2);
    assert_eq!(accumulated.tool_calls.len(), 1);
    assert_eq!(accumulated.tool_calls[0].input["q"], "weather");
    assert!(matches!(accumulated.finish_reason, FinishReason::ToolUse));
    assert_eq!(accumulated.usage.input_tokens, 10);
    assert_eq!(accumulated.usage.output_tokens, 3);
    assert_eq!(accumulated.usage.cache_read_tokens, 2);
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
    const LEGACY: &str = "COS_TEST_GEM_LEGACY_ONLY";
    std::env::set_var(LEGACY, "legacy-key");
    let mut c = AgentConfig::default();
    c.api_key_env = Some(LEGACY.into());
    let gc = GeminiConfig::from_agent_config("gemini-1.5-flash", &c);
    std::env::remove_var(LEGACY);
    assert!(gc.pool.is_none());
    assert_eq!(gc.api_key.as_deref(), Some("legacy-key"));
}

#[test]
fn gemini_pool_built_from_envs() {
    std::env::set_var("COS_TEST_GEM_POOL_A", "gem-aaa");
    std::env::set_var("COS_TEST_GEM_POOL_B", "gem-bbb");
    let mut c = AgentConfig::default();
    c.api_key_envs = vec!["COS_TEST_GEM_POOL_A".into(), "COS_TEST_GEM_POOL_B".into()];
    c.api_key_credential = Some("../ignored-legacy-credential".into());
    let gc = GeminiConfig::from_agent_config("gemini-1.5-flash", &c);
    std::env::remove_var("COS_TEST_GEM_POOL_A");
    std::env::remove_var("COS_TEST_GEM_POOL_B");
    let pool = gc.pool.expect("pool should be built");
    assert_eq!(pool.len(), 2);
    assert!(gc.api_key.is_none());
}

#[test]
fn gemini_pool_unresolved_fails_closed_instead_of_using_single_key() {
    const LEGACY: &str = "COS_TEST_GEM_LEGACY_MUST_NOT_BE_USED";
    const MISSING: &str = "COS_TEST_GEM_POOL_MISSING";
    std::env::set_var(LEGACY, "legacy-secret-must-not-leak");
    std::env::remove_var(MISSING);
    let mut c = AgentConfig::default();
    c.api_key_env = Some(LEGACY.into());
    c.api_key_envs = vec![MISSING.into()];

    let error = GeminiConfig::try_from_agent_config("gemini-1.5-flash", &c).unwrap_err();
    std::env::remove_var(LEGACY);
    match error {
        LlmError::NotConfigured(message) => {
            assert!(message.contains(MISSING), "got: {message}");
            assert!(!message.contains("legacy-secret-must-not-leak"));
        }
        other => panic!("expected NotConfigured, got {other:?}"),
    }
}

#[test]
fn gemini_pool_partial_uses_resolved_entry_and_ignores_single_key() {
    const PRESENT: &str = "COS_TEST_GEM_POOL_PARTIAL_PRESENT";
    const MISSING: &str = "COS_TEST_GEM_POOL_PARTIAL_MISSING";
    std::env::set_var(PRESENT, "pool-key");
    std::env::remove_var(MISSING);
    let mut c = AgentConfig::default();
    c.api_key_credential = Some("../ignored-legacy-credential".into());
    c.api_key_envs = vec![MISSING.into(), PRESENT.into()];

    let gc = GeminiConfig::try_from_agent_config("gemini-1.5-flash", &c)
        .expect("partial pool should resolve");
    std::env::remove_var(PRESENT);
    assert!(gc.api_key.is_none());
    assert_eq!(gc.pool.expect("pool").len(), 1);
}

#[test]
fn gemini_is_configured_true_with_pool_only() {
    std::env::set_var("COS_TEST_GEM_POOL_ICONFIG", "gem-x");
    let mut c = AgentConfig::default();
    c.api_key_envs = vec!["COS_TEST_GEM_POOL_ICONFIG".into()];
    let gc = GeminiConfig::from_agent_config("gemini-1.5-flash", &c);
    std::env::remove_var("COS_TEST_GEM_POOL_ICONFIG");
    let provider = GeminiProvider::new_with_transport(gc, HttpTransport::new().unwrap());
    assert!(provider.is_configured());
}
