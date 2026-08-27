use super::*;

#[test]
fn chat_request_preserves_v0_prompt_wire_shape() {
    let request = ChatRequest {
        prompt: Some("hello".into()),
        session_id: Some("session-1".into()),
        ..ChatRequest::default()
    };
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        r#"{"prompt":"hello","session_id":"session-1"}"#
    );
}

#[test]
fn legacy_messages_request_resolves_latest_user_prompt() {
    let request: ChatRequest = serde_json::from_str(
        r#"{"messages":[{"role":"user","content":"first"},{"role":"assistant","content":"reply"},{"role":"user","content":"latest"}]}"#,
    )
    .unwrap();
    assert_eq!(request.resolved_prompt(), "latest");
}

#[test]
fn response_defaults_accept_older_payloads() {
    let session: SessionSummary =
        serde_json::from_str(r#"{"id":"s","title":"Title"}"#).unwrap();
    assert_eq!(session.message_count, 0);
    let voice: VoiceResponse = serde_json::from_str(
        r#"{"text":"hello","bytes_received":4,"mime_type":"audio/wav"}"#,
    )
    .unwrap();
    assert!(!voice.placeholder);
    assert_eq!(
        serde_json::from_value::<VoiceResponse>(serde_json::to_value(&voice).unwrap()).unwrap(),
        voice
    );
}

#[test]
fn endpoint_response_dtos_round_trip() {
    let session = SessionSummary {
        id: "session-1".into(),
        title: "Session".into(),
        last_ts_ms: Some(10),
        message_count: 2,
    };
    assert_eq!(
        serde_json::from_value::<SessionSummary>(serde_json::to_value(&session).unwrap()).unwrap(),
        session
    );

    let history = HistoryResponse {
        session_id: "session-1".into(),
        n: 1,
        messages: vec![HistoryMessage {
            role: "assistant".into(),
            text: "hello".into(),
            tool_calls: vec![ToolCallView {
                id: "tool-1".into(),
                name: "fs.read".into(),
                input: serde_json::json!({"path": "file"}),
                partial_json: String::new(),
                in_progress: false,
            }],
            tool_results: vec![ToolResultView {
                id: "tool-1".into(),
                name: "fs.read".into(),
                text: "contents".into(),
                is_error: false,
            }],
            ts_ms: 10,
        }],
    };
    assert_eq!(
        serde_json::from_value::<HistoryResponse>(serde_json::to_value(&history).unwrap()).unwrap(),
        history
    );

    let models = ModelsResponse {
        ready: true,
        provider: "provider".into(),
        model: "model".into(),
        label: "Provider - model".into(),
        models: vec![ModelSummary {
            id: "model".into(),
            provider: "provider".into(),
            label: "Model".into(),
        }],
    };
    assert_eq!(
        serde_json::from_value::<ModelsResponse>(serde_json::to_value(&models).unwrap()).unwrap(),
        models
    );

    let cancel = CancelResponse {
        id: "task-1".into(),
        status: "cancelled".into(),
        cancelled: true,
        cancel_requested: false,
        reason: None,
    };
    assert_eq!(
        serde_json::from_value::<CancelResponse>(serde_json::to_value(&cancel).unwrap()).unwrap(),
        cancel
    );
}

#[test]
fn discovery_metadata_has_a_golden_shape_and_valid_range() {
    let endpoint = BridgeEndpoint {
        port: 43123,
        token: "token".into(),
        protocol_version: ProtocolVersion::CURRENT,
        min_protocol_version: ProtocolVersion(crate::MIN_SUPPORTED_PROTOCOL_VERSION),
    };
    assert!(endpoint.has_valid_version_range());
    assert_eq!(
        endpoint.negotiate(ProtocolMetadata::CURRENT),
        Some(ProtocolVersion(1))
    );
    assert_eq!(
        serde_json::to_string(&endpoint).unwrap(),
        r#"{"port":43123,"token":"token","protocol_version":1,"min_protocol_version":1}"#
    );
    assert!(
        serde_json::from_str::<BridgeEndpoint>(
            r#"{"port":43123,"token":"token","protocol_version":1}"#
        )
        .is_err()
    );
}

#[test]
fn stable_error_envelope_has_golden_shape() {
    let error = ErrorEnvelope::new(ErrorCode::InvalidRequest, "bad request")
        .with_hint("Fix the payload.");
    assert_eq!(
        serde_json::to_string(&error).unwrap(),
        r#"{"error":"bad request","code":"invalid_request","hint":"Fix the payload."}"#
    );
    assert_eq!(
        serde_json::from_str::<ErrorEnvelope>(
            r#"{"error":"bad request","code":"invalid_request","hint":"Fix the payload."}"#
        )
        .unwrap(),
        error
    );
}
