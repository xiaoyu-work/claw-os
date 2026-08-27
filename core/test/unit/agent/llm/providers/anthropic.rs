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
    assert_eq!(provider.endpoint(), "https://api.anthropic.com/v1/messages");
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
fn body_filters_reserved_extras_and_preserves_cache_markers() {
    use crate::agent::prompt::caching;

    let mut r = req_text("hi");
    r.extra = serde_json::json!({
        "metadata": {"user_id": "u-1"},
        "_cos_initiator": "agent",
        "_cos_trace": "internal",
        "__private": true
    });
    caching::mark_system_cached(&mut r);

    let body = wire::build_request_body(&r, "claude-3-5-haiku-20241022", false);
    assert_eq!(body["metadata"]["user_id"], "u-1");
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    for key in [
        "_cos_initiator",
        "_cos_trace",
        "__private",
        caching::KEY_SYSTEM,
    ] {
        assert!(body.get(key).is_none(), "reserved extra leaked: {key}");
    }
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
    const LEGACY: &str = "COS_TEST_ANTH_LEGACY_ONLY";
    std::env::set_var(LEGACY, "legacy-key");
    let mut c = AgentConfig::default();
    c.api_key_env = Some(LEGACY.into());
    let ac = AnthropicConfig::from_agent_config("claude-3-5-haiku-20241022", &c);
    std::env::remove_var(LEGACY);
    assert!(ac.pool.is_none());
    assert_eq!(ac.api_key.as_deref(), Some("legacy-key"));
}

#[test]
fn anthropic_pool_built_from_envs() {
    std::env::set_var("COS_TEST_ANTH_POOL_A", "sk-ant-aaa");
    std::env::set_var("COS_TEST_ANTH_POOL_B", "sk-ant-bbb");
    let mut c = AgentConfig::default();
    c.api_key_envs = vec!["COS_TEST_ANTH_POOL_A".into(), "COS_TEST_ANTH_POOL_B".into()];
    c.api_key_credential = Some("../ignored-legacy-credential".into());
    let ac = AnthropicConfig::from_agent_config("claude-3-5-haiku-20241022", &c);
    std::env::remove_var("COS_TEST_ANTH_POOL_A");
    std::env::remove_var("COS_TEST_ANTH_POOL_B");
    let pool = ac.pool.expect("pool should be built");
    assert_eq!(pool.len(), 2);
    assert!(ac.api_key.is_none());
}

#[test]
fn anthropic_pool_unresolved_fails_closed_instead_of_using_single_key() {
    const LEGACY: &str = "COS_TEST_ANTH_LEGACY_MUST_NOT_BE_USED";
    const MISSING: &str = "COS_TEST_ANTH_POOL_MISSING";
    std::env::set_var(LEGACY, "legacy-secret-must-not-leak");
    std::env::remove_var(MISSING);
    let mut c = AgentConfig::default();
    c.api_key_env = Some(LEGACY.into());
    c.api_key_envs = vec![MISSING.into()];

    let error =
        AnthropicConfig::try_from_agent_config("claude-3-5-haiku-20241022", &c).unwrap_err();
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
fn anthropic_pool_partial_uses_resolved_entry_and_ignores_single_key() {
    const PRESENT: &str = "COS_TEST_ANTH_POOL_PARTIAL_PRESENT";
    const MISSING: &str = "COS_TEST_ANTH_POOL_PARTIAL_MISSING";
    std::env::set_var(PRESENT, "pool-key");
    std::env::remove_var(MISSING);
    let mut c = AgentConfig::default();
    c.api_key_credential = Some("../ignored-legacy-credential".into());
    c.api_key_envs = vec![MISSING.into(), PRESENT.into()];

    let ac = AnthropicConfig::try_from_agent_config("claude-3-5-haiku-20241022", &c)
        .expect("partial pool should resolve");
    std::env::remove_var(PRESENT);
    assert!(ac.api_key.is_none());
    assert_eq!(ac.pool.expect("pool").len(), 1);
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
            ev(
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
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
    fn tool_use_assembles_chunked_object_input_and_emits_tool_use() {
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
            ev(
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
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
    fn tool_use_round_trips_array_input() {
        let mut c = wire::StreamConverter::new("m");
        let events = vec![
            ev(
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"batch"}}"#,
            ),
            ev(
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"[{\"value\":1},"}}"#,
            ),
            ev(
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"value\":2}]"}}"#,
            ),
            ev(
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
        ];
        let out = run(&mut c, events.iter());
        let call = out
            .iter()
            .find_map(|event| match event {
                Ok(StreamEvent::ToolUse(call)) => Some(call),
                _ => None,
            })
            .expect("tool use");

        assert_eq!(call.input, serde_json::json!([{"value": 1}, {"value": 2}]));
    }

    #[test]
    fn tool_use_empty_input_defaults_to_object() {
        let mut c = wire::StreamConverter::new("m");
        let events = vec![
            ev(
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"no_args"}}"#,
            ),
            ev(
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
        ];
        let out = run(&mut c, events.iter());
        let call = out
            .iter()
            .find_map(|event| match event {
                Ok(StreamEvent::ToolUse(call)) => Some(call),
                _ => None,
            })
            .expect("tool use");

        assert_eq!(call.input, serde_json::json!({}));
    }

    #[test]
    fn malformed_tool_input_is_terminal_and_never_emits_tool_use() {
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
            ev(
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            ev("message_stop", r#"{"type":"message_stop"}"#),
        ];
        let out = run(&mut c, events.iter());

        assert!(matches!(
            out.last(),
            Some(Err(LlmError::UpstreamMalformed(message)))
                if message.contains("tool_use input")
        ));
        assert!(!out
            .iter()
            .any(|event| matches!(event, Ok(StreamEvent::ToolUse(_)))));
        assert!(!out
            .iter()
            .any(|event| matches!(event, Ok(StreamEvent::Done { .. }))));
        assert!(c.is_finished());
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
            ev(
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
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
        assert!(matches!(
            done,
            StreamEvent::Done {
                finish: FinishReason::Stop,
                ..
            }
        ));
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
        assert!(matches!(
            done,
            StreamEvent::Done {
                finish: FinishReason::Refusal,
                ..
            }
        ));
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

    async fn collect(chunks: Vec<Bytes>) -> Vec<Result<StreamEvent>> {
        collect_with_pool(chunks, None).await
    }

    async fn collect_with_pool(
        chunks: Vec<Bytes>,
        pool: Option<std::sync::Arc<crate::agent::llm::credential_pool::Pool>>,
    ) -> Vec<Result<StreamEvent>> {
        let bytes_stream = stream::iter(chunks.into_iter().map(Ok::<_, reqwest::Error>));
        let lease = pool.as_ref().map(|pool| pool.acquire().unwrap());
        let mut s = wire::AnthropicStream::new(bytes_stream, "claude-x", pool, lease);
        let mut out = Vec::new();
        while let Some(ev) = s.next().await {
            out.push(ev);
        }
        out
    }

    fn stream_test_pool(name: &str) -> std::sync::Arc<crate::agent::llm::credential_pool::Pool> {
        use crate::agent::llm::credential_pool::{Pool, PoolEntry, SelectionStrategy};

        std::sync::Arc::new(
            Pool::from_entries(
                name,
                vec![PoolEntry::inline("test-key")],
                SelectionStrategy::Sticky,
            )
            .unwrap(),
        )
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
        let pool = stream_test_pool("anthropic-valid-terminal");
        let chunks = vec![Bytes::from(body)];
        let out: Vec<StreamEvent> = collect_with_pool(chunks, Some(pool.clone()))
            .await
            .into_iter()
            .map(|r| r.expect("ok"))
            .collect();

        assert_eq!(out.len(), 2, "got: {out:?}");
        assert!(matches!(out[0], StreamEvent::TextDelta { ref text } if text == "OK"));
        assert!(matches!(
            out[1],
            StreamEvent::Done { ref usage, .. }
                if usage.input_tokens == 4 && usage.output_tokens == 1
        ));
        let stats = pool.stats();
        assert_eq!(stats[0].successes, 1);
        assert_eq!(stats[0].failures, 0);
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
    async fn chunked_tool_arguments_round_trip() {
        let body = sse_body(&[
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"weather","input":{}}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\"Sea"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"ttle\",\"units\":[\"c\",\"f\"]}"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);
        let chunks: Vec<Bytes> = body
            .as_bytes()
            .chunks(3)
            .map(Bytes::copy_from_slice)
            .collect();
        let out = collect(chunks).await;
        let call = out
            .iter()
            .find_map(|event| match event {
                Ok(StreamEvent::ToolUse(call)) => Some(call),
                _ => None,
            })
            .expect("tool use");

        assert_eq!(
            call.input,
            serde_json::json!({"city": "Seattle", "units": ["c", "f"]})
        );
        assert!(matches!(
            out.last(),
            Some(Ok(StreamEvent::Done {
                finish: FinishReason::ToolUse,
                ..
            }))
        ));
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
    async fn ping_only_stream_rejects_eof_before_message_stop() {
        let body = sse_body(&[("ping", r#"{"type":"ping"}"#)]);
        let chunks = vec![Bytes::from(body)];
        let out = collect(chunks).await;
        assert!(matches!(
            out.as_slice(),
            [Err(LlmError::UpstreamMalformed(message))]
                if message.contains("message_stop")
        ));
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

    #[tokio::test]
    async fn rejects_truncated_text_before_message_stop() {
        let body = sse_body(&[
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#,
            ),
        ]);
        let pool = stream_test_pool("anthropic-truncated-text");
        let out = collect_with_pool(vec![Bytes::from(body)], Some(pool.clone())).await;

        assert!(matches!(
            out.first(),
            Some(Ok(StreamEvent::TextDelta { text })) if text == "partial"
        ));
        assert!(!out
            .iter()
            .any(|event| matches!(event, Ok(StreamEvent::Done { .. }))));
        assert!(matches!(
            out.last(),
            Some(Err(LlmError::UpstreamMalformed(message)))
                if message.contains("message_stop")
        ));
        let stats = pool.stats();
        assert_eq!(stats[0].successes, 0);
        assert_eq!(stats[0].failures, 1);
    }

    #[tokio::test]
    async fn rejects_completed_tool_before_message_stop() {
        let body = sse_body(&[
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"echo","input":{}}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"text\":\"hi\"}"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}}"#,
            ),
        ]);
        let out = collect(vec![Bytes::from(body)]).await;

        assert!(out
            .iter()
            .any(|event| matches!(event, Ok(StreamEvent::ToolUse(_)))));
        assert!(!out
            .iter()
            .any(|event| matches!(event, Ok(StreamEvent::Done { .. }))));
        assert!(matches!(
            out.last(),
            Some(Err(LlmError::UpstreamMalformed(message)))
                if message.contains("message_stop")
        ));
    }

    #[tokio::test]
    async fn malformed_tool_input_terminates_stream_and_penalizes_pool() {
        let body = sse_body(&[
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"echo","input":{}}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"text\":"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);
        let pool = stream_test_pool("anthropic-malformed-tool-input");
        let out = collect_with_pool(vec![Bytes::from(body)], Some(pool.clone())).await;

        assert!(matches!(
            out.last(),
            Some(Err(LlmError::UpstreamMalformed(message)))
                if message.contains("tool_use input")
        ));
        assert!(!out
            .iter()
            .any(|event| matches!(event, Ok(StreamEvent::ToolUse(_)))));
        assert!(!out
            .iter()
            .any(|event| matches!(event, Ok(StreamEvent::Done { .. }))));
        let stats = pool.stats();
        assert_eq!(stats[0].successes, 0);
        assert_eq!(stats[0].failures, 1);
        assert_eq!(
            stats[0].last_failure_class,
            Some(crate::agent::llm::credential_pool::FailureClass::Transient)
        );
    }

    #[tokio::test]
    async fn rejects_clean_eof_before_message_stop() {
        let out = collect(Vec::new()).await;

        assert!(matches!(
            out.as_slice(),
            [Err(LlmError::UpstreamMalformed(message))]
                if message.contains("message_stop")
        ));
    }
}
