use super::*;
use crate::agent::llm::{FinishReason, Message, Role, Tool, ToolCall, ToolChoice};

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

fn req_image(role: Role, media_type: &str, data: &str) -> ChatRequest {
    let mut request = req_text("unused");
    request.system = None;
    request.messages = vec![Message {
        role,
        content: vec![ContentBlock::Image {
            media_type: media_type.into(),
            data: data.into(),
        }],
    }];
    request
}

// ---- alias / base URL resolution -------------------------------------

#[test]
fn default_base_urls_per_alias() {
    assert!(default_base_url_for("openai").starts_with("https://api.openai.com"));
    assert!(default_base_url_for("xai").starts_with("https://api.x.ai"));
    assert!(default_base_url_for("deepseek").starts_with("https://api.deepseek.com"));
    assert!(default_base_url_for("openrouter").starts_with("https://openrouter.ai"));
    assert!(default_base_url_for("ollama").contains("localhost:11434"));
    // Azure has no universal default — we return empty so the
    // wizard/apply layer can refuse the apply with a clear error.
    assert_eq!(default_base_url_for("azure"), "");
    assert!(default_base_url_for("__unknown__").starts_with("https://api.openai.com"));
}

#[test]
fn azure_is_registered_alias() {
    assert!(is_alias("azure"));
    assert!(PROVIDER_ALIASES.contains(&"azure"));
}

#[test]
fn azure_uses_api_key_header() {
    assert!(alias_uses_api_key_header("azure"));
    assert!(!alias_uses_api_key_header("openai"));
    assert!(!alias_uses_api_key_header("xai"));
}

#[test]
fn azure_provider_not_configured_without_base_url() {
    let mut c = cfg();
    c.api_key_env = Some("DOES_NOT_EXIST_AZURE_KEY".into());
    // base_url not set → falls back to default_base_url_for("azure") = ""
    let provider = OpenAICompatProvider::from_agent_config("azure", "my-deployment", &c);
    assert!(!provider.is_configured());
}

#[tokio::test]
async fn azure_chat_rejects_missing_base_url() {
    let c = cfg();
    let provider = OpenAICompatProvider::from_agent_config("azure", "my-deployment", &c);
    let err = provider.chat(req_text("hi")).await.unwrap_err();
    match err {
        LlmError::NotConfigured(msg) => {
            assert!(msg.contains("azure"), "msg: {msg}");
            assert!(msg.contains("base_url"), "msg: {msg}");
        }
        other => panic!("expected NotConfigured, got {other:?}"),
    }
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
    // Non-azure alias: query string passthrough (e.g. a proxy that
    // requires a routing query). The path is appended in front of
    // the existing query.
    let mut c = cfg();
    c.base_url = Some("https://my.proxy.example.com/v1?route=blue".into());
    let provider = OpenAICompatProvider::from_agent_config("openai", "gpt-4o-mini", &c);
    assert_eq!(
        provider.endpoint(),
        "https://my.proxy.example.com/v1/chat/completions?route=blue"
    );
}

#[test]
fn azure_endpoint_uses_resource_root_and_deployment_name() {
    // The user pastes the resource root from the Azure portal
    // (the same string the Python SDK takes as `azure_endpoint`)
    // and supplies the deployment name via `model`. The provider
    // composes the full `/openai/deployments/<dep>/chat/completions`
    // path itself, mirroring the official SDK behaviour.
    let mut c = cfg();
    c.base_url =
        Some("https://xiaoyu-eastus2.openai.azure.com/?api-version=2024-12-01-preview".into());
    let provider = OpenAICompatProvider::from_agent_config("azure", "gpt-5.4", &c);
    assert_eq!(
            provider.endpoint(),
            "https://xiaoyu-eastus2.openai.azure.com/openai/deployments/gpt-5.4/chat/completions?api-version=2024-12-01-preview"
        );
}

#[test]
fn azure_endpoint_strips_trailing_slash_on_resource_root() {
    let mut c = cfg();
    // Same resource root the user pasted in the wizard, no
    // trailing query.
    c.base_url = Some("https://acme.openai.azure.com/".into());
    let provider = OpenAICompatProvider::from_agent_config("azure", "my-deployment", &c);
    assert_eq!(
        provider.endpoint(),
        "https://acme.openai.azure.com/openai/deployments/my-deployment/chat/completions"
    );
}

#[test]
fn azure_endpoint_handles_resource_root_without_trailing_slash() {
    let mut c = cfg();
    c.base_url = Some("https://acme.openai.azure.com?api-version=2024-12-01-preview".into());
    let provider = OpenAICompatProvider::from_agent_config("azure", "gpt-5.4", &c);
    assert_eq!(
            provider.endpoint(),
            "https://acme.openai.azure.com/openai/deployments/gpt-5.4/chat/completions?api-version=2024-12-01-preview"
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

#[test]
fn responses_endpoint_preserves_query_string() {
    assert_eq!(
        endpoint_for_wire_api(
            "https://api.individual.githubcopilot.com?region=west",
            crate::agent::llm::providers::copilot_auth::CopilotWireApi::Responses,
        ),
        "https://api.individual.githubcopilot.com/responses?region=west"
    );
}

#[test]
fn copilot_headers_include_responses_api_contract() {
    let request = with_copilot_headers(
        reqwest::Client::new().post("https://api.individual.githubcopilot.com/responses"),
        true,
        crate::agent::llm::providers::copilot_auth::COPILOT_INITIATOR_USER,
    )
    .build()
    .unwrap();
    let headers = request.headers();
    assert_eq!(
        headers["X-GitHub-Api-Version"],
        crate::agent::llm::providers::copilot_auth::GITHUB_API_VERSION
    );
    assert_eq!(
        headers["X-Initiator"],
        crate::agent::llm::providers::copilot_auth::COPILOT_INITIATOR_USER
    );
    assert_eq!(
        headers["X-Interaction-Type"],
        crate::agent::llm::providers::copilot_auth::COPILOT_INTERACTION_TYPE
    );
    assert_eq!(
        headers["OpenAI-Intent"],
        crate::agent::llm::providers::copilot_auth::COPILOT_INTERACTION_TYPE
    );
    assert_eq!(headers["Copilot-Vision-Request"], "true");
}

#[test]
fn copilot_initiator_distinguishes_user_and_tool_follow_up() {
    let user_request = req_text("hello");
    assert_eq!(
        copilot_initiator(&user_request),
        crate::agent::llm::providers::copilot_auth::COPILOT_INITIATOR_USER
    );

    let mut automatic_request = req_text("summarise");
    automatic_request.extra =
        serde_json::json!({"_cos_initiator": "agent", "seed": 7});
    let body =
        wire::build_request_body(&automatic_request, "gpt-4o-mini", false).unwrap();
    assert!(body.get("_cos_initiator").is_none());
    assert_eq!(body["seed"], 7);
    assert_eq!(
        copilot_initiator(&automatic_request),
        crate::agent::llm::providers::copilot_auth::COPILOT_INITIATOR_AGENT
    );

    let mut tool_request = req_text("unused");
    tool_request.messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".into(),
            is_error: false,
            content: "done".into(),
        }],
    }];
    assert_eq!(
        copilot_initiator(&tool_request),
        crate::agent::llm::providers::copilot_auth::COPILOT_INITIATOR_AGENT
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
    std::env::set_var("COS_TEST_KEY_VAR_8743", " \t\r\n ");
    let v = resolve_api_key(None, Some("COS_TEST_KEY_VAR_8743")).unwrap();
    assert_eq!(v, None);
    std::env::remove_var("COS_TEST_KEY_VAR_8743");
}

#[test]
fn resolve_api_key_trims_env_value() {
    std::env::set_var("COS_TEST_KEY_VAR_8744", "  sk-from-env  \n");
    let v = resolve_api_key(None, Some("COS_TEST_KEY_VAR_8744")).unwrap();
    assert_eq!(v.as_deref(), Some("sk-from-env"));
    std::env::remove_var("COS_TEST_KEY_VAR_8744");
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
    let body = wire::build_request_body(&r, "gpt-4o-mini", false).unwrap();
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
fn chat_completions_serializes_mixed_images_and_preserves_tool_history() {
    let mut request = req_text("unused");
    request.messages = vec![
        Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "before".into(),
                },
                ContentBlock::Image {
                    media_type: "image/jpeg".into(),
                    data: "/9j/2Q==".into(),
                },
                ContentBlock::Text {
                    text: "between".into(),
                },
                ContentBlock::Image {
                    media_type: "image/png".into(),
                    data: "iVBORw0KGgo=".into(),
                },
                ContentBlock::Text {
                    text: "after".into(),
                },
            ],
        },
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "checking".into(),
                },
                ContentBlock::ToolUse {
                    id: "call_vision".into(),
                    name: "inspect".into(),
                    input: serde_json::json!({"mode": "detailed"}),
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_vision".into(),
                is_error: false,
                content: "done".into(),
            }],
        },
    ];

    // OpenAI-compatible and Azure requests always take this branch.
    // Copilot catalogue entries advertising /chat/completions do too.
    let body = build_wire_request_body(
        &request,
        "gpt-4o",
        false,
        crate::agent::llm::providers::copilot_auth::CopilotWireApi::ChatCompletions,
        compatibility_for_alias("openai"),
    )
    .unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(
        messages[1]["content"],
        serde_json::json!([
            {"type": "text", "text": "before"},
            {
                "type": "image_url",
                "image_url": {
                    "url": "data:image/jpeg;base64,/9j/2Q==",
                    "detail": "auto"
                }
            },
            {"type": "text", "text": "between"},
            {
                "type": "image_url",
                "image_url": {
                    "url": "data:image/png;base64,iVBORw0KGgo=",
                    "detail": "auto"
                }
            },
            {"type": "text", "text": "after"}
        ])
    );
    assert_eq!(messages[2]["content"], "checking");
    assert_eq!(messages[2]["tool_calls"][0]["id"], "call_vision");
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call_vision");
    assert_eq!(messages[3]["content"], "done");
}

#[test]
fn chat_completions_rejects_malformed_or_unsupported_images() {
    for (media_type, data, expected) in [
        ("image/png", "", "must not be empty"),
        ("image/jpeg", "not base64!", "not valid base64"),
        ("image/bmp", "Qk0=", "does not support image media type"),
    ] {
        let error = wire::build_request_body(
            &req_image(Role::User, media_type, data),
            "gpt-4o",
            false,
        )
        .unwrap_err();
        assert!(
            matches!(error, LlmError::InvalidRequest(ref message) if message.contains(expected)),
            "unexpected error for {media_type}: {error:?}"
        );
    }
}

#[test]
fn chat_completions_rejects_images_in_non_user_messages() {
    let error = wire::build_request_body(
        &req_image(Role::Assistant, "image/png", "iVBORw0KGgo="),
        "gpt-4o",
        false,
    )
    .unwrap_err();
    assert!(
        matches!(error, LlmError::InvalidRequest(ref message) if message.contains("user messages")),
        "unexpected error: {error:?}"
    );
}

#[test]
fn chat_completions_rejects_known_non_vision_models() {
    let error = wire::build_request_body(
        &req_image(Role::User, "image/png", "iVBORw0KGgo="),
        "deepseek-chat",
        false,
    )
    .unwrap_err();
    assert!(
        matches!(error, LlmError::InvalidRequest(ref message) if message.contains("does not support image input")),
        "unexpected error: {error:?}"
    );
}

#[test]
fn builds_responses_body_with_tool_history() {
    let mut request = req_text("unused");
    request.messages = vec![
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    id: "rs_42".into(),
                    summary: vec!["Need to inspect the requested file.".into()],
                    encrypted_content: Some("opaque-ciphertext".into()),
                },
                ContentBlock::Text {
                    text: "I'll inspect it.".into(),
                },
                ContentBlock::ToolState {
                    tool_use_id: "call_42".into(),
                    thought_signature: "opaque-thought-signature".into(),
                },
                ContentBlock::ToolUse {
                    id: "call_42".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "/tmp/a"}),
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_42".into(),
                is_error: false,
                content: "hello".into(),
            }],
        },
    ];
    request.tools = vec![Tool {
        name: "read_file".into(),
        description: "Read a file".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }),
    }];
    request.tool_choice = ToolChoice::Tool {
        name: "read_file".into(),
    };
    request.extra = serde_json::json!({
        "store": true,
        "include": ["not.allowed"],
        "seed": 42,
        "_cos_initiator": "agent",
        "_cos_trace": "internal",
        "__cache_system": true,
        "__private": true
    });

    let body = responses_wire::build_request_body(&request, "gpt-5.6-sol", true);
    assert_eq!(body["model"], "gpt-5.6-sol");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["include"][0], "reasoning.encrypted_content");
    assert_eq!(body["seed"], 42);
    for key in [
        "_cos_initiator",
        "_cos_trace",
        "__cache_system",
        "__private",
    ] {
        assert!(body.get(key).is_none(), "reserved extra leaked: {key}");
    }
    assert_eq!(body["max_output_tokens"], 64);
    assert!(body.get("temperature").is_none());
    assert_eq!(body["input"][0]["type"], "message");
    assert_eq!(body["input"][0]["role"], "system");
    assert_eq!(body["input"][1]["type"], "reasoning");
    assert_eq!(body["input"][1]["id"], "rs_42");
    assert_eq!(body["input"][1]["encrypted_content"], "opaque-ciphertext");
    assert_eq!(body["input"][2]["content"][0]["type"], "output_text");
    assert_eq!(body["input"][3]["type"], "function_call");
    assert_eq!(body["input"][3]["call_id"], "call_42");
    assert_eq!(
        body["input"][3]["thought_signature"],
        "opaque-thought-signature"
    );
    assert_eq!(body["input"][4]["type"], "function_call_output");
    assert_eq!(body["input"][4]["output"], "hello");
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "read_file");
    assert!(body["tools"][0].get("function").is_none());
    assert_eq!(body["tool_choice"]["type"], "function");
    assert_eq!(body["tool_choice"]["name"], "read_file");
}

#[test]
fn responses_does_not_replay_reasoning_without_encrypted_state() {
    let mut request = req_text("unused");
    request.system = None;
    request.messages = vec![Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Reasoning {
            id: "rs_without_ciphertext".into(),
            summary: vec!["Visible summary only".into()],
            encrypted_content: None,
        }],
    }];
    let body = responses_wire::build_request_body(&request, "gpt-5.6-sol", false);
    assert!(
        body["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["type"] != "reasoning"),
        "reasoning without encrypted_content must not be replayed"
    );
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
    let body = wire::build_request_body(&r, "gpt-5.4-mini", false).unwrap();
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
    let body = wire::build_request_body(&r, "gpt-4o-mini", false).unwrap();
    assert_eq!(body["tools"][0]["function"]["name"], "echo");
    assert_eq!(body["tool_choice"], "auto");
}

#[test]
fn body_marks_stream_when_requested() {
    let r = req_text("hi");
    let body = wire::build_request_body(&r, "m", true).unwrap();
    assert_eq!(body["stream"], true);
}

#[test]
fn official_openai_stream_requests_terminal_usage() {
    let mut request = req_text("hi");
    request.extra = serde_json::json!({
        "stream_options": {"include_usage": false}
    });
    let body = build_wire_request_body(
        &request,
        "gpt-4o-mini",
        true,
        crate::agent::llm::providers::copilot_auth::CopilotWireApi::ChatCompletions,
        compatibility_for_alias("openai"),
    )
    .unwrap();
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);

    let non_streaming = build_wire_request_body(
        &request,
        "gpt-4o-mini",
        false,
        crate::agent::llm::providers::copilot_auth::CopilotWireApi::ChatCompletions,
        compatibility_for_alias("openai"),
    )
    .unwrap();
    assert!(non_streaming.get("stream_options").is_none());
}

#[test]
fn compatibility_alias_streams_omit_unsupported_stream_options() {
    for alias in PROVIDER_ALIASES
        .iter()
        .copied()
        .filter(|alias| *alias != "openai")
    {
        let mut request = req_text("hi");
        request.extra = serde_json::json!({
            "seed": 7,
            "stream_options": {"include_usage": true}
        });
        let body = build_wire_request_body(
            &request,
            "compat-model",
            true,
            crate::agent::llm::providers::copilot_auth::CopilotWireApi::ChatCompletions,
            compatibility_for_alias(alias),
        )
        .unwrap();
        assert_eq!(body["stream"], true, "alias: {alias}");
        assert_eq!(body["seed"], 7, "alias: {alias}");
        assert!(
            body.get("stream_options").is_none(),
            "strict compatibility alias {alias} received stream_options: {body}"
        );
    }
}

#[test]
fn responses_requests_do_not_inherit_chat_stream_options() {
    let body = build_wire_request_body(
        &req_text("hi"),
        "gpt-5.6-sol",
        true,
        crate::agent::llm::providers::copilot_auth::CopilotWireApi::Responses,
        AliasCompatibility::OFFICIAL_OPENAI,
    )
    .unwrap();
    assert_eq!(body["stream"], true);
    assert!(body.get("stream_options").is_none());
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
    let body = wire::build_request_body(&r, "m", false).unwrap();
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
    let body = wire::build_request_body(&r, "m", false).unwrap();
    let tool_msg = &body["messages"][2];
    assert_eq!(tool_msg["role"], "tool");
    assert_eq!(tool_msg["tool_call_id"], "call_1");
    assert_eq!(tool_msg["content"], "{\"ok\":true}");
}

#[test]
fn body_fans_out_multiple_tool_results_into_separate_tool_messages() {
    // Regression for Azure 400 "tool_call_ids did not have
    // response messages" when the assistant calls multiple
    // tools in one turn. The runtime aggregates all tool
    // results into a single User message containing several
    // ToolResult blocks; the wire serializer must emit each
    // one as its own `role=tool` message with the matching
    // tool_call_id, otherwise the conversation history is
    // malformed.
    let mut r = req_text("inventory");
    r.messages.push(crate::agent::llm::Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::ToolUse {
                id: "call_A".into(),
                name: "mounts".into(),
                input: serde_json::json!({}),
            },
            ContentBlock::ToolUse {
                id: "call_B".into(),
                name: "recent".into(),
                input: serde_json::json!({"limit": 50}),
            },
        ],
    });
    r.messages.push(crate::agent::llm::Message {
        role: Role::User,
        content: vec![
            ContentBlock::ToolResult {
                tool_use_id: "call_A".into(),
                is_error: false,
                content: "{\"mounts\":[]}".into(),
            },
            ContentBlock::ToolResult {
                tool_use_id: "call_B".into(),
                is_error: false,
                content: "{\"files\":[]}".into(),
            },
        ],
    });
    let body = wire::build_request_body(&r, "m", false).unwrap();
    let msgs = body["messages"].as_array().expect("messages array");
    // system + user "inventory" + assistant with two tool_calls
    // + two role=tool messages = 5 total.
    assert_eq!(msgs.len(), 5, "got: {msgs:?}");
    let tool_a = &msgs[3];
    let tool_b = &msgs[4];
    assert_eq!(tool_a["role"], "tool");
    assert_eq!(tool_a["tool_call_id"], "call_A");
    assert_eq!(tool_a["content"], "{\"mounts\":[]}");
    assert_eq!(tool_b["role"], "tool");
    assert_eq!(tool_b["tool_call_id"], "call_B");
    assert_eq!(tool_b["content"], "{\"files\":[]}");
}

#[test]
fn body_filters_reserved_extras_and_preserves_provider_extras() {
    let mut r = req_text("hi");
    r.extra = serde_json::json!({
        "seed": 42,
        "response_format": {"type":"json_object"},
        "_cos_initiator": "agent",
        "_cos_trace": "internal",
        "__cache_tools": true,
        "__private": true
    });
    let body = wire::build_request_body(&r, "m", false).unwrap();
    assert_eq!(body["seed"], 42);
    assert_eq!(body["response_format"]["type"], "json_object");
    for key in [
        "_cos_initiator",
        "_cos_trace",
        "__cache_tools",
        "__private",
    ] {
        assert!(body.get(key).is_none(), "reserved extra leaked: {key}");
    }
}

// ---- response parsing ------------------------------------------------

#[test]
fn parses_responses_text_tool_and_usage() {
    let raw = br#"{
            "model": "gpt-5.6-sol",
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "Checking."}]
                },
                {
                    "type": "reasoning",
                    "id": "rs_1",
                    "summary": [{
                        "type": "summary_text",
                        "text": "Inspect the file before answering."
                    }],
                    "encrypted_content": "opaque-ciphertext"
                },
                {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"/tmp/a\"}",
                    "thought_signature": "opaque-thought-signature"
                }
            ],
            "usage": {
                "input_tokens": 12,
                "output_tokens": 7,
                "input_tokens_details": {"cached_tokens": 4}
            }
        }"#;
    let chat = responses_wire::response_from_slice(raw, "fallback").unwrap();
    assert_eq!(chat.model, "gpt-5.6-sol");
    assert_eq!(chat.finish_reason, FinishReason::ToolUse);
    assert_eq!(chat.tool_calls.len(), 1);
    assert_eq!(chat.tool_calls[0].id, "call_1");
    assert_eq!(chat.tool_calls[0].name, "read_file");
    assert_eq!(chat.tool_calls[0].input["path"], "/tmp/a");
    assert_eq!(chat.usage.input_tokens, 12);
    assert_eq!(chat.usage.output_tokens, 7);
    assert_eq!(chat.usage.cache_read_tokens, 4);
    assert!(matches!(
        &chat.content[0],
        ContentBlock::Text { text } if text == "Checking."
    ));
    assert!(matches!(
        &chat.content[1],
        ContentBlock::Reasoning {
            id,
            encrypted_content: Some(encrypted),
            ..
        } if id == "rs_1" && encrypted == "opaque-ciphertext"
    ));
    assert!(matches!(
        &chat.content[2],
        ContentBlock::ToolState {
            tool_use_id,
            thought_signature,
        } if tool_use_id == "call_1" && thought_signature == "opaque-thought-signature"
    ));
    assert!(matches!(
        &chat.content[3],
        ContentBlock::ToolUse { id, name, .. }
            if id == "call_1" && name == "read_file"
    ));
}

#[test]
fn parses_responses_incomplete_and_refusal() {
    let incomplete = br#"{
            "status": "incomplete",
            "output": [],
            "incomplete_details": {"reason": "max_output_tokens"}
        }"#;
    let chat = responses_wire::response_from_slice(incomplete, "m").unwrap();
    assert_eq!(chat.finish_reason, FinishReason::Length);

    let refusal = br#"{
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{"type": "refusal", "refusal": "Cannot comply."}]
            }]
        }"#;
    let chat = responses_wire::response_from_slice(refusal, "m").unwrap();
    assert_eq!(chat.finish_reason, FinishReason::Refusal);
    assert!(matches!(
        &chat.content[0],
        ContentBlock::Text { text } if text == "Cannot comply."
    ));
}

#[test]
fn responses_error_codes_preserve_retry_semantics() {
    let rate_limited = br#"{
            "status": "failed",
            "error": {
                "code": "rate_limit_exceeded",
                "message": "too many requests"
            },
            "output": []
        }"#;
    assert!(matches!(
        responses_wire::response_from_slice(rate_limited, "m"),
        Err(LlmError::RateLimited { .. })
    ));

    let server_error = br#"{
            "status": "failed",
            "error": {
                "code": "server_error",
                "message": "temporary failure"
            },
            "output": []
        }"#;
    assert!(matches!(
        responses_wire::response_from_slice(server_error, "m"),
        Err(LlmError::Provider { status: 500, .. })
    ));
}

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
    let err = wire::classify_http_error(reqwest::StatusCode::from_u16(403).unwrap(), b"forbidden");
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

fn request_json_body(request: &[u8]) -> serde_json::Value {
    let body_start = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
        .expect("HTTP request has a header terminator");
    serde_json::from_slice(&request[body_start..]).expect("HTTP request body is JSON")
}

#[derive(Clone, Copy)]
struct MockReply {
    status_line: &'static str,
    content_type: &'static str,
    body: &'static str,
}

async fn spawn_sequence_mock(
    replies: Vec<MockReply>,
) -> (String, tokio::task::JoinHandle<Vec<Vec<u8>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");

    let handle = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut requests = Vec::with_capacity(replies.len());
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::with_capacity(4096);
            let mut buffer = [0u8; 4096];
            let mut header_end = None;
            let mut content_length = 0usize;
            loop {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if header_end.is_none() {
                    if let Some(position) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        header_end = Some(position + 4);
                        let headers = std::str::from_utf8(&request[..position]).unwrap_or_default();
                        for line in headers.split("\r\n") {
                            if let Some(value) =
                                line.to_ascii_lowercase().strip_prefix("content-length:")
                            {
                                content_length = value.trim().parse().unwrap_or_default();
                            }
                        }
                    }
                }
                if header_end.is_some_and(|start| request.len() - start >= content_length) {
                    break;
                }
            }

            let response = format!(
                "{}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.status_line,
                reply.content_type,
                reply.body.len(),
                reply.body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
            requests.push(request);
        }
        requests
    });
    (url, handle)
}

fn request_header(request: &[u8], name: &str) -> Option<String> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?;
    std::str::from_utf8(&request[..header_end])
        .ok()?
        .split("\r\n")
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .find_map(|(header_name, value)| {
            header_name
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
}

struct FakeCopilotAuthSource {
    initial: crate::agent::llm::providers::copilot_auth::CopilotToken,
    refreshed: crate::agent::llm::providers::copilot_auth::CopilotToken,
    reject_first_model_call: bool,
    ensure_calls: std::sync::atomic::AtomicUsize,
    refresh_calls: std::sync::atomic::AtomicUsize,
    model_calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl CopilotAuthSource for FakeCopilotAuthSource {
    async fn ensure_token(
        &self,
        github_token: &str,
    ) -> CopilotAuthResult<crate::agent::llm::providers::copilot_auth::CopilotToken> {
        assert_eq!(github_token, "github-long-lived");
        self.ensure_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.initial.clone())
    }

    async fn refresh_rejected_token(
        &self,
        github_token: &str,
        rejected_token: &crate::agent::llm::providers::copilot_auth::CopilotToken,
    ) -> CopilotAuthResult<crate::agent::llm::providers::copilot_auth::CopilotToken> {
        assert_eq!(github_token, "github-long-lived");
        assert_eq!(rejected_token.bearer, self.initial.bearer);
        self.refresh_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.refreshed.clone())
    }

    async fn wire_api_for_model(
        &self,
        _token: &crate::agent::llm::providers::copilot_auth::CopilotToken,
        model: &str,
    ) -> CopilotAuthResult<crate::agent::llm::providers::copilot_auth::CopilotWireApi> {
        assert_eq!(model, "gpt-4o-mini");
        let call = self
            .model_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.reject_first_model_call && call == 0 {
            return Err(
                crate::agent::llm::providers::copilot_auth::CopilotAuthError::Http {
                    status: 401,
                    body: "expired exchanged token".into(),
                },
            );
        }
        Ok(crate::agent::llm::providers::copilot_auth::CopilotWireApi::ChatCompletions)
    }
}

fn copilot_provider_for_retry_test(
    base_url: &str,
    reject_first_model_call: bool,
) -> (
    OpenAICompatProvider,
    Arc<crate::agent::llm::credential_pool::Pool>,
    Arc<FakeCopilotAuthSource>,
) {
    use crate::agent::llm::credential_pool::{Pool, PoolEntry, SelectionStrategy};

    let pool = Arc::new(
        Pool::from_entries(
            "copilot-issue-17",
            vec![PoolEntry::inline("github-long-lived")],
            SelectionStrategy::Sticky,
        )
        .unwrap(),
    );
    let source = Arc::new(FakeCopilotAuthSource {
        initial: crate::agent::llm::providers::copilot_auth::CopilotToken {
            bearer: "copilot-rejected".into(),
            base_url: base_url.into(),
            expires_at_unix: u64::MAX,
        },
        refreshed: crate::agent::llm::providers::copilot_auth::CopilotToken {
            bearer: "copilot-refreshed".into(),
            base_url: base_url.into(),
            expires_at_unix: u64::MAX,
        },
        reject_first_model_call,
        ensure_calls: std::sync::atomic::AtomicUsize::new(0),
        refresh_calls: std::sync::atomic::AtomicUsize::new(0),
        model_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let config = OpenAICompatConfig {
        alias: "copilot".into(),
        base_url: base_url.into(),
        api_key: None,
        model: "gpt-4o-mini".into(),
        extra_headers: std::collections::HashMap::new(),
        request_timeout: std::time::Duration::from_secs(5),
        pool: Some(pool.clone()),
    };
    (
        OpenAICompatProvider::new_with_copilot_auth_source(config, source.clone()),
        pool,
        source,
    )
}

fn assert_copilot_retry_bearers(requests: &[Vec<u8>]) {
    assert_eq!(requests.len(), 2);
    assert_eq!(
        request_header(&requests[0], "authorization").as_deref(),
        Some("Bearer copilot-rejected")
    );
    assert_eq!(
        request_header(&requests[1], "authorization").as_deref(),
        Some("Bearer copilot-refreshed")
    );
}

fn assert_single_pool_success(pool: &crate::agent::llm::credential_pool::Pool) {
    let stats = pool.stats();
    assert_eq!(stats[0].successes, 1);
    assert_eq!(stats[0].failures, 0);
    assert!(stats[0].cooldown_remaining_ms.is_none());
}

fn assert_single_pool_auth_failure(pool: &crate::agent::llm::credential_pool::Pool) {
    use crate::agent::llm::credential_pool::FailureClass;

    let stats = pool.stats();
    assert_eq!(stats[0].successes, 0);
    assert_eq!(stats[0].failures, 1);
    assert_eq!(
        stats[0].last_failure_class,
        Some(FailureClass::CooldownWorthy)
    );
    assert!(stats[0].cooldown_remaining_ms.is_some());
}

#[tokio::test]
async fn copilot_chat_retries_rejected_exchanged_token_without_cooling_github_credential() {
    let (base_url, server) = spawn_sequence_mock(vec![
        MockReply {
            status_line: "HTTP/1.1 401 Unauthorized",
            content_type: "application/json",
            body: r#"{"error":{"message":"expired exchanged token"}}"#,
        },
        MockReply {
            status_line: "HTTP/1.1 200 OK",
            content_type: "application/json",
            body: r#"{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"ok"}}]}"#,
        },
    ])
    .await;
    let (provider, pool, source) = copilot_provider_for_retry_test(&base_url, false);

    let response = provider.chat(req_text("hello")).await.unwrap();
    assert!(matches!(
        response.content.first(),
        Some(ContentBlock::Text { text }) if text == "ok"
    ));
    let requests = server.await.unwrap();
    assert_copilot_retry_bearers(&requests);
    assert_eq!(
        source
            .refresh_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_single_pool_success(&pool);
}

#[tokio::test]
async fn copilot_chat_persistent_auth_failure_retries_once_and_cools_github_credential() {
    let (base_url, server) = spawn_sequence_mock(vec![
        MockReply {
            status_line: "HTTP/1.1 401 Unauthorized",
            content_type: "application/json",
            body: r#"{"error":{"message":"expired exchanged token"}}"#,
        },
        MockReply {
            status_line: "HTTP/1.1 403 Forbidden",
            content_type: "application/json",
            body: r#"{"error":{"message":"not authorized"}}"#,
        },
    ])
    .await;
    let (provider, pool, source) = copilot_provider_for_retry_test(&base_url, false);

    let error = provider.chat(req_text("hello")).await.unwrap_err();
    assert!(matches!(error, LlmError::Auth));
    let requests = server.await.unwrap();
    assert_copilot_retry_bearers(&requests);
    assert_eq!(
        source
            .refresh_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_single_pool_auth_failure(&pool);
}

#[tokio::test]
async fn copilot_stream_retries_rejected_exchanged_token_without_cooling_github_credential() {
    use futures_util::StreamExt;

    let (base_url, server) = spawn_sequence_mock(vec![
        MockReply {
            status_line: "HTTP/1.1 403 Forbidden",
            content_type: "application/json",
            body: r#"{"error":{"message":"expired exchanged token"}}"#,
        },
        MockReply {
            status_line: "HTTP/1.1 200 OK",
            content_type: "text/event-stream",
            body: concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            ),
        },
    ])
    .await;
    let (provider, pool, source) = copilot_provider_for_retry_test(&base_url, false);

    let events: Vec<_> = provider
        .chat_stream(req_text("hello"))
        .await
        .unwrap()
        .collect()
        .await;
    assert!(matches!(
        events.last(),
        Some(Ok(StreamEvent::Done {
            finish: FinishReason::Stop,
            ..
        }))
    ));
    let requests = server.await.unwrap();
    assert_copilot_retry_bearers(&requests);
    assert_eq!(
        source
            .refresh_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_single_pool_success(&pool);
}

#[tokio::test]
async fn copilot_stream_persistent_auth_failure_retries_once() {
    let (base_url, server) = spawn_sequence_mock(vec![
        MockReply {
            status_line: "HTTP/1.1 403 Forbidden",
            content_type: "application/json",
            body: r#"{"error":{"message":"expired exchanged token"}}"#,
        },
        MockReply {
            status_line: "HTTP/1.1 401 Unauthorized",
            content_type: "application/json",
            body: r#"{"error":{"message":"still unauthorized"}}"#,
        },
    ])
    .await;
    let (provider, pool, source) = copilot_provider_for_retry_test(&base_url, false);

    let result = provider.chat_stream(req_text("hello")).await;
    assert!(matches!(result, Err(LlmError::Auth)));
    let requests = server.await.unwrap();
    assert_copilot_retry_bearers(&requests);
    assert_eq!(
        source
            .refresh_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_single_pool_auth_failure(&pool);
}

#[tokio::test]
async fn copilot_model_catalog_auth_rejection_refreshes_before_request() {
    let (base_url, server) = spawn_sequence_mock(vec![MockReply {
        status_line: "HTTP/1.1 200 OK",
        content_type: "application/json",
        body: r#"{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"ok"}}]}"#,
    }])
    .await;
    let (provider, pool, source) = copilot_provider_for_retry_test(&base_url, true);

    provider.chat(req_text("hello")).await.unwrap();
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_header(&requests[0], "authorization").as_deref(),
        Some("Bearer copilot-refreshed")
    );
    assert_eq!(
        source
            .refresh_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(
        source.model_calls.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    assert_single_pool_success(&pool);
}

#[tokio::test]
async fn official_openai_stream_serializes_usage_option_and_reports_usage() {
    use futures_util::StreamExt;

    let response_body = concat!(
        "data: {\"model\":\"gpt-4o-mini\",\"choices\":[{\"delta\":{\"content\":\"ok\"},",
        "\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":17,\"completion_tokens\":3}}\n\n",
        "data: [DONE]\n\n"
    );
    let (base_url, handle) = spawn_one_shot_mock("HTTP/1.1 200 OK", response_body).await;
    let mut config = AgentConfig::default();
    config.base_url = Some(base_url);
    config.request_timeout = 5;
    let provider = OpenAICompatProvider::from_agent_config("openai", "gpt-4o-mini", &config);

    let events: Vec<_> = provider
        .chat_stream(req_text("hello"))
        .await
        .expect("stream request succeeds")
        .collect()
        .await;
    assert!(matches!(
        events.last(),
        Some(Ok(StreamEvent::Done { usage, .. }))
            if usage.input_tokens == 17 && usage.output_tokens == 3
    ));

    let request = handle.await.unwrap();
    let body = request_json_body(&request);
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn strict_compat_stream_omits_option_and_keeps_missing_usage_explicit() {
    use futures_util::StreamExt;

    let response_body = concat!(
        "data: {\"model\":\"llama3\",\"choices\":[{\"delta\":{\"content\":\"ok\"},",
        "\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let (base_url, handle) = spawn_one_shot_mock("HTTP/1.1 200 OK", response_body).await;
    let mut config = AgentConfig::default();
    config.base_url = Some(base_url);
    config.request_timeout = 5;
    let provider = OpenAICompatProvider::from_agent_config("ollama", "llama3", &config);

    let events: Vec<_> = provider
        .chat_stream(req_text("hello"))
        .await
        .expect("strict compatibility stream succeeds")
        .collect()
        .await;
    assert!(matches!(
        events.last(),
        Some(Ok(StreamEvent::Done { usage, .. }))
            if usage.input_tokens == 0 && usage.output_tokens == 0
    ));

    let request = handle.await.unwrap();
    let body = request_json_body(&request);
    assert_eq!(body["stream"], true);
    assert!(body.get("stream_options").is_none());
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
async fn azure_alias_sends_api_key_header_not_bearer() {
    let response_body = r#"{
            "id":"x","object":"chat.completion","created":1,
            "model":"my-deployment",
            "choices":[{"index":0,"finish_reason":"stop",
                "message":{"role":"assistant","content":"hi"}}],
            "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
        }"#;
    let (base_url, handle) = spawn_one_shot_mock("HTTP/1.1 200 OK", response_body).await;
    // base_url ends in `/v1` from the mock helper — for the
    // assertion that matters (which header is sent) the exact
    // path doesn't matter, just that the request goes out.

    let mut c = AgentConfig::default();
    c.base_url = Some(base_url);
    c.api_key_env = Some("COS_TEST_AZURE_KEY".into());
    c.request_timeout = 5;
    std::env::set_var("COS_TEST_AZURE_KEY", "az-secret-123");

    let provider = OpenAICompatProvider::from_agent_config("azure", "my-deployment", &c);
    let _ = provider.chat(req_text("hi")).await;

    let request_bytes = handle.await.unwrap();
    let request = String::from_utf8_lossy(&request_bytes);
    let lower = request.to_lowercase();
    assert!(
        lower.contains("api-key: az-secret-123"),
        "expected Azure api-key header, got headers:\n{}",
        request
    );
    assert!(
        !lower.contains("authorization: bearer"),
        "Azure should not send Authorization: Bearer, got headers:\n{}",
        request
    );

    std::env::remove_var("COS_TEST_AZURE_KEY");
}

#[tokio::test]
async fn openai_alias_still_sends_bearer_not_api_key() {
    let response_body = r#"{
            "id":"x","object":"chat.completion","created":1,
            "model":"gpt-4o-mini",
            "choices":[{"index":0,"finish_reason":"stop",
                "message":{"role":"assistant","content":"hi"}}]
        }"#;
    let (base_url, handle) = spawn_one_shot_mock("HTTP/1.1 200 OK", response_body).await;

    let mut c = AgentConfig::default();
    c.base_url = Some(base_url);
    c.api_key_env = Some("COS_TEST_OPENAI_BEARER_KEY".into());
    c.request_timeout = 5;
    std::env::set_var("COS_TEST_OPENAI_BEARER_KEY", "sk-openai");

    let provider = OpenAICompatProvider::from_agent_config("openai", "gpt-4o-mini", &c);
    let _ = provider.chat(req_text("hi")).await;

    let request = String::from_utf8_lossy(&handle.await.unwrap()).to_lowercase();
    assert!(request.contains("authorization: bearer sk-openai"));
    assert!(!request.contains("api-key: "));

    std::env::remove_var("COS_TEST_OPENAI_BEARER_KEY");
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
    const LEGACY: &str = "COS_TEST_OPENAI_LEGACY_ONLY";
    std::env::set_var(LEGACY, "legacy-key");
    let mut c = AgentConfig::default();
    c.api_key_env = Some(LEGACY.into());
    let oc = OpenAICompatConfig::from_agent_config("openai", "gpt-4o-mini", &c);
    std::env::remove_var(LEGACY);
    assert!(oc.pool.is_none());
    assert_eq!(oc.api_key.as_deref(), Some("legacy-key"));
}

#[test]
fn pool_built_from_envs() {
    std::env::set_var("COS_TEST_POOL_KEY_A", "sk-aaa");
    std::env::set_var("COS_TEST_POOL_KEY_B", "sk-bbb");
    let mut c = AgentConfig::default();
    c.api_key_envs = vec!["COS_TEST_POOL_KEY_A".into(), "COS_TEST_POOL_KEY_B".into()];
    c.api_key_credential = Some("../ignored-legacy-credential".into());
    let oc = OpenAICompatConfig::from_agent_config("openai", "gpt-4o-mini", &c);
    std::env::remove_var("COS_TEST_POOL_KEY_A");
    std::env::remove_var("COS_TEST_POOL_KEY_B");
    let pool = oc.pool.expect("pool should be built");
    assert_eq!(pool.len(), 2);
    assert!(oc.api_key.is_none());
}

#[test]
fn pool_unresolved_fails_closed_instead_of_using_single_key() {
    const LEGACY: &str = "COS_TEST_OPENAI_LEGACY_MUST_NOT_BE_USED";
    const MISSING: &str = "COS_TEST_DOES_NOT_EXIST_ENV_AAAA";
    std::env::set_var(LEGACY, "legacy-secret-must-not-leak");
    std::env::remove_var(MISSING);
    let mut c = AgentConfig::default();
    c.api_key_env = Some(LEGACY.into());
    c.api_key_envs = vec![MISSING.into()];
    let error =
        OpenAICompatConfig::try_from_agent_config("openai", "gpt-4o-mini", &c).unwrap_err();
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
fn pool_partial_uses_resolved_entry_and_ignores_single_key() {
    const PRESENT: &str = "COS_TEST_OPENAI_POOL_PARTIAL_PRESENT";
    const MISSING: &str = "COS_TEST_OPENAI_POOL_PARTIAL_MISSING";
    std::env::set_var(PRESENT, "pool-key");
    std::env::remove_var(MISSING);
    let mut c = AgentConfig::default();
    c.api_key_credential = Some("../ignored-legacy-credential".into());
    c.api_key_envs = vec![MISSING.into(), PRESENT.into()];

    let oc = OpenAICompatConfig::try_from_agent_config("openai", "gpt-4o-mini", &c)
        .expect("partial pool should resolve");
    std::env::remove_var(PRESENT);
    assert!(oc.api_key.is_none());
    assert_eq!(oc.pool.expect("pool").len(), 1);
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

#[test]
fn responses_stream_emits_text_tool_and_terminal_usage() {
    use crate::agent::llm::sse::SseEvent;

    let event = |data: serde_json::Value| SseEvent {
        event: "message".into(),
        data: data.to_string(),
    };
    let mut converter = responses_wire::ResponsesStreamConverter::new("gpt-5.6-sol".into());

    let reasoning = converter.process(&event(serde_json::json!({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": {
            "type": "reasoning",
            "id": "rs_1",
            "summary": [{
                "type": "summary_text",
                "text": "Need to inspect the file."
            }],
            "encrypted_content": "opaque-ciphertext"
        }
    })));
    assert!(matches!(
        &reasoning[0],
        Ok(StreamEvent::Reasoning {
            id,
            encrypted_content: Some(content),
            ..
        }) if id == "rs_1" && content == "opaque-ciphertext"
    ));

    let text = converter.process(&event(serde_json::json!({
        "type": "response.output_text.delta",
        "item_id": "msg_1",
        "output_index": 0,
        "delta": "Hello"
    })));
    assert!(matches!(
        &text[0],
        Ok(StreamEvent::TextDelta { text }) if text == "Hello"
    ));

    let start = converter.process(&event(serde_json::json!({
        "type": "response.output_item.added",
        "output_index": 1,
        "item": {
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "read_file",
            "arguments": "",
            "thought_signature": "opaque-thought-signature"
        }
    })));
    assert!(matches!(
        &start[0],
        Ok(StreamEvent::ToolUseStart { id, name })
            if id == "call_1" && name == "read_file"
    ));

    let delta = converter.process(&event(serde_json::json!({
        "type": "response.function_call_arguments.delta",
        "output_index": 1,
        "delta": "{\"path\":\"/tmp/a\"}"
    })));
    assert!(matches!(
        &delta[0],
        Ok(StreamEvent::ToolInputDelta { id, partial_json })
            if id == "call_1" && partial_json == "{\"path\":\"/tmp/a\"}"
    ));

    let done = converter.process(&event(serde_json::json!({
        "type": "response.output_item.done",
        "output_index": 1,
        "item": {
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "read_file",
            "arguments": "{\"path\":\"/tmp/a\"}",
            "thought_signature": "opaque-thought-signature"
        }
    })));
    assert!(matches!(
        &done[0],
        Ok(StreamEvent::ToolState {
            tool_use_id,
            thought_signature,
        }) if tool_use_id == "call_1" && thought_signature == "opaque-thought-signature"
    ));
    assert!(matches!(
        &done[1],
        Ok(StreamEvent::ToolUse(ToolCall { id, name, input }))
            if id == "call_1" && name == "read_file" && input["path"] == "/tmp/a"
    ));

    let terminal = converter.process(&event(serde_json::json!({
        "type": "response.completed",
        "response": {
            "model": "gpt-5.6-sol",
            "status": "completed",
            "output": [{
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "read_file",
                "arguments": "{\"path\":\"/tmp/a\"}",
                "thought_signature": "opaque-thought-signature"
            }],
            "usage": {
                "input_tokens": 20,
                "output_tokens": 5,
                "input_tokens_details": {"cached_tokens": 3}
            }
        }
    })));
    assert_eq!(
        terminal
            .iter()
            .filter(|event| matches!(event, Ok(StreamEvent::ToolUse(_))))
            .count(),
        0,
        "terminal response must not re-emit an already completed tool call"
    );
    assert!(matches!(
        terminal.last(),
        Some(Ok(StreamEvent::Done { finish, usage }))
            if *finish == FinishReason::ToolUse
                && usage.input_tokens == 20
                && usage.output_tokens == 5
                && usage.cache_read_tokens == 3
    ));
}

#[tokio::test]
async fn responses_stream_rejects_eof_without_terminal_event() {
    use futures_util::StreamExt;

    let body = bytes::Bytes::from_static(
        b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
    );
    let bytes = futures_util::stream::iter(vec![Ok::<_, reqwest::Error>(body)]);
    let stream = responses_wire::ResponsesStream::new(bytes, "gpt-5.6-sol".into(), None, None);
    let events: Vec<_> = stream.collect().await;
    assert!(matches!(
        events.last(),
        Some(Err(LlmError::UpstreamMalformed(message)))
            if message.contains("before a terminal event")
    ));
}

#[test]
fn responses_done_marker_reports_tool_use_finish() {
    use crate::agent::llm::sse::SseEvent;

    let event = |data: serde_json::Value| SseEvent {
        event: "message".into(),
        data: data.to_string(),
    };
    let mut converter = responses_wire::ResponsesStreamConverter::new("gpt-5.6-sol".into());
    converter.process(&event(serde_json::json!({
        "type": "response.output_item.added",
        "output_index": 0,
        "item": {
            "type": "function_call",
            "call_id": "call_1",
            "name": "read_file",
            "arguments": "{}"
        }
    })));
    let done = converter.process(&SseEvent {
        event: "message".into(),
        data: "[DONE]".into(),
    });
    assert!(matches!(
        done.last(),
        Some(Ok(StreamEvent::Done {
            finish: FinishReason::ToolUse,
            ..
        }))
    ));
}

#[test]
fn pool_classifies_copilot_preflight_auth_failures() {
    assert_eq!(
        pool_failure_class(&LlmError::Auth),
        crate::agent::llm::credential_pool::FailureClass::CooldownWorthy
    );
    assert_eq!(
        pool_failure_class(&LlmError::UpstreamMalformed("catalog".into())),
        crate::agent::llm::credential_pool::FailureClass::Transient
    );
}

/// HIGH-4: the streaming converter must surface each delta as
/// soon as it parses, not buffer them all until [DONE]. This
/// exercises `OpenAiStreamConverter::process` directly: feed
/// three deltas + [DONE] and assert the output order is
/// `TextDelta("Hel")`, `TextDelta("lo, ")`, `TextDelta("world!")`,
/// `Done`.
#[test]
fn streaming_emits_incrementally() {
    use crate::agent::llm::sse::SseEvent;
    let mut conv = wire::OpenAiStreamConverter::new("gpt-4o-mini".into());

    let mk = |body: &str| SseEvent {
        event: "message".into(),
        data: body.into(),
    };

    let mut out: Vec<StreamEvent> = Vec::new();
    for chunk in [
        r#"{"choices":[{"delta":{"content":"Hel"},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"content":"lo, "},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"content":"world!"},"finish_reason":"stop"}]}"#,
    ] {
        for e in conv.process(&mk(chunk)) {
            out.push(e.expect("delta should parse"));
        }
    }
    // The text deltas must have surfaced BEFORE we see [DONE].
    let texts: Vec<&str> = out
        .iter()
        .filter_map(|e| match e {
            StreamEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec!["Hel", "lo, ", "world!"],
        "deltas should stream in order"
    );
    // No Done yet — it only arrives on [DONE] / finish_stream.
    assert!(
        !out.iter().any(|e| matches!(e, StreamEvent::Done { .. })),
        "Done event must wait for [DONE]"
    );
    // Now feed [DONE].
    for e in conv.process(&mk("[DONE]")) {
        out.push(e.expect("done should parse"));
    }
    let last = out.last().expect("at least one event");
    match last {
        StreamEvent::Done { finish, .. } => assert!(matches!(finish, FinishReason::Stop)),
        other => panic!("expected Done, got {other:?}"),
    }
    // And the converter is now poisoned: further events are noops.
    assert!(conv.is_finished());
    assert!(conv.process(&mk("{}")).is_empty());
}

/// Malformed JSON in a streaming chunk must surface as
/// `LlmError::UpstreamMalformed`, NOT as a silently dropped
/// delta. The converter must also poison itself so subsequent
/// chunks don't keep emitting.
#[test]
fn streaming_malformed_chunk_errors() {
    use crate::agent::llm::sse::SseEvent;
    let mut conv = wire::OpenAiStreamConverter::new("gpt-4o-mini".into());
    let sse = SseEvent {
        event: "message".into(),
        data: "this is not json".into(),
    };
    let out = conv.process(&sse);
    assert_eq!(out.len(), 1);
    assert!(
        matches!(out[0], Err(LlmError::UpstreamMalformed(_))),
        "got {:?}",
        out[0]
    );
    assert!(conv.is_finished());
}

async fn collect_openai_chat_stream(chunks: Vec<bytes::Bytes>) -> Vec<Result<StreamEvent>> {
    use futures_util::StreamExt;

    let bytes = futures_util::stream::iter(chunks.into_iter().map(Ok::<_, reqwest::Error>));
    wire::OpenAiStream::new(bytes, "gpt-4o-mini".into(), None, None)
        .collect()
        .await
}

fn openai_stream_test_pool(name: &str) -> std::sync::Arc<crate::agent::llm::credential_pool::Pool> {
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
async fn openai_chat_stream_rejects_truncated_text_before_done() {
    use futures_util::StreamExt;

    let pool = openai_stream_test_pool("openai-truncated-text");
    let lease = pool.acquire().unwrap();
    let body = bytes::Bytes::from_static(
        b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"stop\"}]}\n\n",
    );
    let bytes = futures_util::stream::iter(vec![Ok::<_, reqwest::Error>(body)]);
    let events: Vec<_> =
        wire::OpenAiStream::new(bytes, "gpt-4o-mini".into(), Some(pool.clone()), Some(lease))
            .collect()
            .await;

    assert!(matches!(
        events.first(),
        Some(Ok(StreamEvent::TextDelta { text })) if text == "partial"
    ));
    assert!(!events
        .iter()
        .any(|event| matches!(event, Ok(StreamEvent::Done { .. }))));
    assert!(matches!(
        events.last(),
        Some(Err(LlmError::UpstreamMalformed(message)))
            if message.contains("[DONE]")
    ));
    let stats = pool.stats();
    assert_eq!(stats[0].successes, 0);
    assert_eq!(stats[0].failures, 1);
}

#[tokio::test]
async fn openai_chat_stream_rejects_completed_tool_before_done() {
    let body = bytes::Bytes::from_static(
        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"echo\",\"arguments\":\"{\\\"text\\\":\\\"hi\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
    );
    let events = collect_openai_chat_stream(vec![body]).await;

    assert!(events
        .iter()
        .any(|event| matches!(event, Ok(StreamEvent::ToolUseStart { .. }))));
    assert!(events
        .iter()
        .any(|event| matches!(event, Ok(StreamEvent::ToolInputDelta { .. }))));
    assert!(!events
        .iter()
        .any(|event| matches!(event, Ok(StreamEvent::ToolUse(_)))));
    assert!(!events
        .iter()
        .any(|event| matches!(event, Ok(StreamEvent::Done { .. }))));
    assert!(matches!(
        events.last(),
        Some(Err(LlmError::UpstreamMalformed(message)))
            if message.contains("[DONE]")
    ));
}

#[tokio::test]
async fn openai_chat_stream_rejects_clean_eof_before_done() {
    let events = collect_openai_chat_stream(Vec::new()).await;

    assert_eq!(events.len(), 1);
    assert!(matches!(
        events.first(),
        Some(Err(LlmError::UpstreamMalformed(message)))
            if message.contains("[DONE]")
    ));
}

#[tokio::test]
async fn openai_chat_stream_preserves_terminal_usage_and_pool_success() {
    use futures_util::StreamExt;

    let pool = openai_stream_test_pool("openai-valid-terminal");
    let lease = pool.acquire().unwrap();
    let body = bytes::Bytes::from_static(
        b"data: {\"model\":\"gpt-valid\",\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n",
    );
    let bytes = futures_util::stream::iter(vec![Ok::<_, reqwest::Error>(body)]);
    let events: Vec<_> =
        wire::OpenAiStream::new(bytes, "gpt-4o-mini".into(), Some(pool.clone()), Some(lease))
            .collect()
            .await;

    assert!(matches!(
        events.last(),
        Some(Ok(StreamEvent::Done { usage, .. }))
            if usage.input_tokens == 11 && usage.output_tokens == 2
    ));
    let stats = pool.stats();
    assert_eq!(stats[0].successes, 1);
    assert_eq!(stats[0].failures, 0);
}
