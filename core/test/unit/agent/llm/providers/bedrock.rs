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
    let p = BedrockProvider::from_agent_config("anthropic.claude-3-5-sonnet-20241022-v2:0", &c);
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
    assert_eq!(
        v.get("system").and_then(|s| s.as_str()),
        Some("you are helpful")
    );
    // max_tokens preserved.
    assert_eq!(v.get("max_tokens").and_then(|m| m.as_u64()), Some(64));
    // messages array survives.
    assert!(v.get("messages").and_then(|m| m.as_array()).is_some());
}

#[test]
fn body_filters_reserved_extras_and_preserves_provider_cache_fields() {
    use crate::agent::prompt::caching;

    let mut request = req_text("hello");
    request.extra = serde_json::json!({
        "top_k": 32,
        "_cos_initiator": "agent",
        "_cos_trace": "internal",
        "__private": true
    });
    caching::mark_system_cached(&mut request);

    let body_bytes = build_bedrock_body_bytes(&request).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["top_k"], 32);
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
    let err =
        classify_bedrock_error(reqwest::StatusCode::from_u16(429).unwrap(), b"", None, None);
    assert!(matches!(
        err,
        LlmError::RateLimited {
            retry_after_ms: 1000
        }
    ));
}

#[test]
fn http_403_without_amz_type_still_auth() {
    let err =
        classify_bedrock_error(reqwest::StatusCode::from_u16(403).unwrap(), b"", None, None);
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
    assert_eq!(url_encode_path_segment("AbZ-_.~09"), "AbZ-_.~09");
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
    let p =
        BedrockProvider::from_agent_config("anthropic.claude-3-haiku-20240307-v1:0", &cfg());
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

// ---- Streaming URL & headers ----------------------------------------

#[test]
fn stream_model_path_uses_invoke_with_response_stream_suffix() {
    let mut c = cfg();
    c.aws_access_key_env = Some("COS_BR_STR_AK1".into());
    c.aws_secret_key_env = Some("COS_BR_STR_SK1".into());
    std::env::set_var("COS_BR_STR_AK1", "AKID");
    std::env::set_var("COS_BR_STR_SK1", "secret");
    let p = BedrockProvider::from_agent_config("anthropic.claude-3-5-sonnet-20241022-v2:0", &c);
    assert_eq!(
        p.stream_model_path(),
        "/model/anthropic.claude-3-5-sonnet-20241022-v2%3A0/invoke-with-response-stream"
    );
    std::env::remove_var("COS_BR_STR_AK1");
    std::env::remove_var("COS_BR_STR_SK1");
}

#[test]
fn stream_model_path_encodes_arn_with_slashes_and_colons() {
    // Bedrock accepts full provisioned-model ARNs as model IDs.
    // Path-segment encoding must escape both `/` and `:`.
    let mut c = cfg();
    c.aws_access_key_env = Some("COS_BR_STR_AK2".into());
    c.aws_secret_key_env = Some("COS_BR_STR_SK2".into());
    std::env::set_var("COS_BR_STR_AK2", "AKID");
    std::env::set_var("COS_BR_STR_SK2", "secret");
    let arn = "arn:aws:bedrock:us-east-1:123:provisioned-model/abc";
    let p = BedrockProvider::from_agent_config(arn, &c);
    let path = p.stream_model_path();
    assert!(
        path.contains("arn%3Aaws%3Abedrock%3Aus-east-1%3A123%3Aprovisioned-model%2Fabc"),
        "ARN must be fully path-segment-encoded; got {path}"
    );
    assert!(path.ends_with("/invoke-with-response-stream"));
    std::env::remove_var("COS_BR_STR_AK2");
    std::env::remove_var("COS_BR_STR_SK2");
}

#[test]
fn stream_full_url_combines_base_and_stream_path() {
    let mut c = cfg();
    c.aws_region = Some("eu-west-1".into());
    c.aws_access_key_env = Some("COS_BR_STR_AK3".into());
    c.aws_secret_key_env = Some("COS_BR_STR_SK3".into());
    std::env::set_var("COS_BR_STR_AK3", "AKID");
    std::env::set_var("COS_BR_STR_SK3", "secret");
    let p = BedrockProvider::from_agent_config("anthropic.claude-foo", &c);
    assert_eq!(
        p.stream_full_url(),
        "https://bedrock-runtime.eu-west-1.amazonaws.com/model/anthropic.claude-foo/invoke-with-response-stream"
    );
    std::env::remove_var("COS_BR_STR_AK3");
    std::env::remove_var("COS_BR_STR_SK3");
}

// ---- Streamed exception classifier ----------------------------------

#[test]
fn classify_throttling_lower_camel() {
    let e = stream_wire::classify_streamed_exception("throttlingException", "");
    assert!(matches!(e, LlmError::RateLimited { .. }), "got {e:?}");
}

#[test]
fn classify_throttling_pascal_case_defensive_fallback() {
    // Some non-conforming clients emit PascalCase; our matcher
    // normalises the leading char so we still recognise it.
    let e = stream_wire::classify_streamed_exception("ThrottlingException", "");
    assert!(matches!(e, LlmError::RateLimited { .. }), "got {e:?}");
}

#[test]
fn classify_validation_with_message_preserves_message() {
    let e =
        stream_wire::classify_streamed_exception("validationException", "max tokens exceeded");
    match e {
        LlmError::InvalidRequest(m) => {
            assert!(m.contains("max tokens exceeded"), "got {m}");
            assert!(m.contains("validationException"), "got {m}");
        }
        other => panic!("expected InvalidRequest, got {other:?}"),
    }
}

#[test]
fn classify_model_stream_error_to_provider_500() {
    let e =
        stream_wire::classify_streamed_exception("modelStreamErrorException", "decoder OOM");
    assert!(
        matches!(e, LlmError::Provider { status: 500, ref message } if message.contains("decoder OOM"))
    );
}

#[test]
fn classify_model_timeout_to_provider_504() {
    let e = stream_wire::classify_streamed_exception(
        "modelTimeoutException",
        "model failed to respond within 30s",
    );
    assert!(matches!(e, LlmError::Provider { status: 504, .. }));
}

#[test]
fn classify_internal_server_to_provider_500() {
    let e = stream_wire::classify_streamed_exception("internalServerException", "");
    assert!(matches!(e, LlmError::Provider { status: 500, .. }));
}

#[test]
fn classify_service_unavailable_to_provider_503() {
    let e = stream_wire::classify_streamed_exception("serviceUnavailableException", "");
    assert!(matches!(e, LlmError::Provider { status: 503, .. }));
}

#[test]
fn classify_unknown_exception_surfaces_as_provider_500_and_includes_name() {
    // Unknown exception names must NOT be silently swallowed —
    // surface them so observability picks up new AWS-side error
    // taxonomy expansions.
    let e =
        stream_wire::classify_streamed_exception("newSurpriseException", "explanatory text");
    match e {
        LlmError::Provider { status, message } => {
            assert_eq!(status, 500);
            assert!(message.contains("newSurpriseException"), "got {message}");
            assert!(message.contains("explanatory text"), "got {message}");
        }
        other => panic!("expected Provider, got {other:?}"),
    }
}

// ---- BedrockStream end-to-end (synthetic frames) --------------------

use crate::agent::llm::aws_eventstream::encode_frame;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use bytes::Bytes;
use futures_util::stream as futstream;
use futures_util::StreamExt;

/// Build one event-frame whose inner SSE data field is `inner_json`.
fn event_frame_json(inner_json: &str) -> Vec<u8> {
    let outer = serde_json::json!({
        "bytes": B64.encode(inner_json),
    });
    encode_frame(
        &[(":message-type", "event"), (":event-type", "chunk")],
        outer.to_string().as_bytes(),
    )
}

fn anthropic_event_json(kind: &str, extra: serde_json::Value) -> String {
    let mut obj = match extra {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert("type".into(), serde_json::Value::String(kind.into()));
    serde_json::Value::Object(obj).to_string()
}

fn collect(
    body: Vec<Vec<u8>>,
) -> Vec<crate::agent::llm::Result<crate::agent::llm::StreamEvent>> {
    let chunks: Vec<std::result::Result<Bytes, reqwest::Error>> =
        body.into_iter().map(|v| Ok(Bytes::from(v))).collect();
    let s = futstream::iter(chunks);
    let stream = stream_wire::BedrockStream::new(s, "claude-3-5-sonnet-20241022");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async { stream.collect::<Vec<_>>().await })
}

#[test]
fn stream_handles_full_text_lifecycle_with_message_stop() {
    let frames = vec![
        event_frame_json(&anthropic_event_json(
            "message_start",
            serde_json::json!({
                "message": {
                    "id": "msg_1",
                    "model": "claude-3-5-sonnet-20241022",
                    "usage": { "input_tokens": 12, "output_tokens": 0 }
                }
            }),
        )),
        event_frame_json(&anthropic_event_json(
            "content_block_start",
            serde_json::json!({
                "index": 0,
                "content_block": { "type": "text", "text": "" }
            }),
        )),
        event_frame_json(&anthropic_event_json(
            "content_block_delta",
            serde_json::json!({
                "index": 0,
                "delta": { "type": "text_delta", "text": "Hello" }
            }),
        )),
        event_frame_json(&anthropic_event_json(
            "content_block_delta",
            serde_json::json!({
                "index": 0,
                "delta": { "type": "text_delta", "text": " world" }
            }),
        )),
        event_frame_json(&anthropic_event_json(
            "content_block_stop",
            serde_json::json!({ "index": 0 }),
        )),
        event_frame_json(&anthropic_event_json(
            "message_delta",
            serde_json::json!({
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 7 }
            }),
        )),
        event_frame_json(&anthropic_event_json("message_stop", serde_json::json!({}))),
    ];
    let events = collect(frames);
    let oks: Vec<_> = events.iter().map(|r| r.as_ref().unwrap()).collect();
    // Expect text deltas + final Done (no Message in streaming).
    let text: String = oks
        .iter()
        .filter_map(|e| match e {
            crate::agent::llm::StreamEvent::TextDelta { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello world");
    let usage = oks.iter().find_map(|event| match event {
        crate::agent::llm::StreamEvent::Done { usage, .. } => Some(usage),
        _ => None,
    });
    let usage = usage.unwrap_or_else(|| panic!("expected Done event; got {oks:?}"));
    assert_eq!(usage.input_tokens, 12);
    assert_eq!(usage.output_tokens, 7);
}

#[test]
fn stream_emits_tool_input_delta_then_final_tool_use() {
    let frames = vec![
        event_frame_json(&anthropic_event_json(
            "message_start",
            serde_json::json!({
                "message": {
                    "id": "msg_2",
                    "model": "claude-3-5-sonnet-20241022",
                    "usage": { "input_tokens": 5, "output_tokens": 0 }
                }
            }),
        )),
        event_frame_json(&anthropic_event_json(
            "content_block_start",
            serde_json::json!({
                "index": 0,
                "content_block": {
                    "type": "tool_use",
                    "id": "toolu_xyz",
                    "name": "echo",
                    "input": {}
                }
            }),
        )),
        event_frame_json(&anthropic_event_json(
            "content_block_delta",
            serde_json::json!({
                "index": 0,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": "{\"text\":\"hi"
                }
            }),
        )),
        event_frame_json(&anthropic_event_json(
            "content_block_delta",
            serde_json::json!({
                "index": 0,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": "\"}"
                }
            }),
        )),
        event_frame_json(&anthropic_event_json(
            "content_block_stop",
            serde_json::json!({ "index": 0 }),
        )),
        event_frame_json(&anthropic_event_json(
            "message_delta",
            serde_json::json!({
                "delta": { "stop_reason": "tool_use" },
                "usage": { "output_tokens": 9 }
            }),
        )),
        event_frame_json(&anthropic_event_json("message_stop", serde_json::json!({}))),
    ];
    let events = collect(frames);
    let oks: Vec<_> = events.iter().map(|r| r.as_ref().unwrap()).collect();
    // Expect at least one ToolUseStart, ToolInputDelta(s), ToolUse final.
    assert!(
        oks.iter()
            .any(|e| matches!(e, crate::agent::llm::StreamEvent::ToolUseStart { .. })),
        "missing ToolUseStart in {oks:?}"
    );
    let final_tool_use = oks.iter().find_map(|e| match e {
        crate::agent::llm::StreamEvent::ToolUse(tc) => Some(tc),
        _ => None,
    });
    let tc = final_tool_use.expect("missing final ToolUse event");
    assert_eq!(tc.name, "echo");
    assert_eq!(tc.id, "toolu_xyz");
    assert_eq!(tc.input, serde_json::json!({"text": "hi"}));
}

#[test]
fn stream_exception_frame_maps_to_rate_limited() {
    let frames = vec![encode_frame(
        &[
            (":message-type", "exception"),
            (":exception-type", "throttlingException"),
            (":content-type", "application/json"),
        ],
        br#"{"message":"rate exceeded"}"#,
    )];
    let events = collect(frames);
    // Last (only) event must be a RateLimited error.
    assert_eq!(events.len(), 1);
    let err = events[0].as_ref().unwrap_err();
    assert!(matches!(err, LlmError::RateLimited { .. }), "got {err:?}");
}

#[test]
fn stream_exception_frame_unknown_name_surfaces_provider_error() {
    let frames = vec![encode_frame(
        &[
            (":message-type", "exception"),
            (":exception-type", "newCloudExpansionException"),
            (":content-type", "application/json"),
        ],
        br#"{"message":"future taxonomy"}"#,
    )];
    let events = collect(frames);
    assert_eq!(events.len(), 1);
    let err = events[0].as_ref().unwrap_err();
    match err {
        LlmError::Provider { status, message } => {
            assert_eq!(*status, 500);
            assert!(
                message.contains("newCloudExpansionException"),
                "got {message}"
            );
        }
        other => panic!("expected Provider, got {other:?}"),
    }
}

#[test]
fn stream_unmodeled_error_frame_maps_to_provider() {
    let frames = vec![encode_frame(
        &[
            (":message-type", "error"),
            (":error-code", "InternalServerError"),
            (":error-message", "an error has occurred"),
        ],
        b"",
    )];
    let events = collect(frames);
    assert_eq!(events.len(), 1);
    let err = events[0].as_ref().unwrap_err();
    match err {
        LlmError::Provider { status, message } => {
            assert_eq!(*status, 500);
            assert!(message.contains("InternalServerError"), "got {message}");
            assert!(message.contains("an error has occurred"), "got {message}");
        }
        other => panic!("expected Provider, got {other:?}"),
    }
}

#[test]
fn stream_unknown_message_type_is_silently_ignored_for_forward_compat() {
    // An unrecognised :message-type that arrives BEFORE the
    // real terminator (message_stop) must not poison the stream.
    let frames = vec![
        encode_frame(
            &[(":message-type", "futureKindWeDontKnow")],
            b"opaque payload",
        ),
        event_frame_json(&anthropic_event_json(
            "message_start",
            serde_json::json!({
                "message": {
                    "id": "msg_3",
                    "model": "claude-3-5-sonnet-20241022",
                    "usage": { "input_tokens": 1, "output_tokens": 0 }
                }
            }),
        )),
        event_frame_json(&anthropic_event_json(
            "message_delta",
            serde_json::json!({
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 1 }
            }),
        )),
        event_frame_json(&anthropic_event_json("message_stop", serde_json::json!({}))),
    ];
    let events = collect(frames);
    let oks: Vec<_> = events.iter().map(|r| r.as_ref().unwrap()).collect();
    assert!(
        oks.iter()
            .any(|e| matches!(e, crate::agent::llm::StreamEvent::Done { .. })),
        "stream should still complete normally; got {oks:?}"
    );
}

#[test]
fn stream_rejects_truncated_text_before_message_stop() {
    let frames = vec![
        event_frame_json(&anthropic_event_json(
            "content_block_delta",
            serde_json::json!({
                "index": 0,
                "delta": { "type": "text_delta", "text": "partial" }
            }),
        )),
        event_frame_json(&anthropic_event_json(
            "message_delta",
            serde_json::json!({
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 3 }
            }),
        )),
    ];
    let events = collect(frames);

    assert!(matches!(
        events.first(),
        Some(Ok(crate::agent::llm::StreamEvent::TextDelta { text }))
            if text == "partial"
    ));
    assert!(!events
        .iter()
        .any(|event| matches!(event, Ok(crate::agent::llm::StreamEvent::Done { .. }))));
    assert!(matches!(
        events.last(),
        Some(Err(LlmError::UpstreamMalformed(message)))
            if message.contains("message_stop")
    ));
}

#[test]
fn stream_rejects_completed_tool_before_message_stop() {
    let frames = vec![
        event_frame_json(&anthropic_event_json(
            "content_block_start",
            serde_json::json!({
                "index": 0,
                "content_block": {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "echo",
                    "input": {}
                }
            }),
        )),
        event_frame_json(&anthropic_event_json(
            "content_block_delta",
            serde_json::json!({
                "index": 0,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": "{\"text\":\"hi\"}"
                }
            }),
        )),
        event_frame_json(&anthropic_event_json(
            "content_block_stop",
            serde_json::json!({ "index": 0 }),
        )),
        event_frame_json(&anthropic_event_json(
            "message_delta",
            serde_json::json!({
                "delta": { "stop_reason": "tool_use" },
                "usage": { "output_tokens": 4 }
            }),
        )),
    ];
    let events = collect(frames);

    assert!(events
        .iter()
        .any(|event| matches!(event, Ok(crate::agent::llm::StreamEvent::ToolUse(_)))));
    assert!(!events
        .iter()
        .any(|event| matches!(event, Ok(crate::agent::llm::StreamEvent::Done { .. }))));
    assert!(matches!(
        events.last(),
        Some(Err(LlmError::UpstreamMalformed(message)))
            if message.contains("message_stop")
    ));
}

#[test]
fn stream_rejects_clean_eof_before_message_stop() {
    let events = collect(Vec::new());

    assert!(matches!(
        events.as_slice(),
        [Err(LlmError::UpstreamMalformed(message))]
            if message.contains("message_stop")
    ));
}

#[test]
fn stream_truncated_body_at_eof_surfaces_stream_error() {
    // Build a complete frame, then chop off the final 4 bytes so
    // the parser sees a truncated tail at EOF.
    let mut frame = event_frame_json(&anthropic_event_json(
        "message_start",
        serde_json::json!({
            "message": {
                "id": "msg_4",
                "model": "claude-3-5-sonnet-20241022",
                "usage": { "input_tokens": 1, "output_tokens": 0 }
            }
        }),
    ));
    let n = frame.len();
    frame.truncate(n - 4);
    let events = collect(vec![frame]);
    // Should contain at least one Stream error at the end.
    let last = events.last().expect("at least one event");
    let err = last.as_ref().unwrap_err();
    assert!(
        matches!(err, LlmError::Stream(_)),
        "expected Stream error; got {err:?}"
    );
}

#[test]
fn stream_bad_message_crc_surfaces_stream_error_and_terminates() {
    let mut frame = event_frame_json(&anthropic_event_json(
        "message_start",
        serde_json::json!({
            "message": {
                "id": "msg_5",
                "model": "claude-3-5-sonnet-20241022",
                "usage": { "input_tokens": 1, "output_tokens": 0 }
            }
        }),
    ));
    // Corrupt the message CRC (last 4 bytes are the trailer).
    let n = frame.len();
    frame[n - 1] ^= 0xff;
    // Even if more frames follow, the stream must terminate at
    // the corrupt frame with an error.
    let events = collect(vec![frame, event_frame_json("{}")]);
    let any_err = events.iter().any(|r| matches!(r, Err(LlmError::Stream(_))));
    assert!(
        any_err,
        "expected at least one Stream error; got {events:?}"
    );
}
