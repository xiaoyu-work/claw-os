use super::*;

fn binding() -> AbiBinding {
    AbiBinding {
        task_id: "task-a".to_string(),
        session_id: "session-a".to_string(),
        owner_uid: 1000,
        extension_id: "observer".to_string(),
        extension_version: "1.0.0".to_string(),
        package_digest: format!("sha256:{}", "a".repeat(64)),
        manifest_digest: "b".repeat(64),
        entry_digest: "c".repeat(64),
        capability_generation: "d".repeat(16),
        lease_digest: "e".repeat(64),
        instance_nonce: "f".repeat(64),
        additive: BTreeMap::new(),
    }
}

fn initialize() -> AbiRequest {
    AbiRequest {
        protocol: ABI_VERSION,
        binding: binding(),
        sequence: 0,
        message: HostMessage::Initialize {
            min_version: ABI_VERSION,
            max_version: ABI_VERSION,
            required_features: vec![FEATURE_OBSERVATIONAL_EVENTS.to_string()],
            subscriptions: vec![EventKind::SessionStart],
            requested_capability_count: 0,
        },
        additive: BTreeMap::new(),
    }
}

#[test]
fn protocol_downgrade_and_binding_substitution_fail_closed() {
    let request = initialize();
    let mut response = AbiResponse {
        protocol: ABI_VERSION,
        binding: binding(),
        sequence: 0,
        message: ExtensionMessage::Ready {
            selected_version: ABI_VERSION,
            accepted_features: vec![FEATURE_OBSERVATIONAL_EVENTS.to_string()],
        },
        additive: BTreeMap::new(),
    };
    validate_ready(
        &request,
        &response,
        ABI_VERSION,
        ABI_VERSION,
        &[FEATURE_OBSERVATIONAL_EVENTS.to_string()],
    )
    .unwrap();
    response.protocol = 0;
    if let ExtensionMessage::Ready {
        selected_version, ..
    } = &mut response.message
    {
        *selected_version = 0;
    }
    assert!(validate_ready(
        &request,
        &response,
        ABI_VERSION,
        ABI_VERSION,
        &[FEATURE_OBSERVATIONAL_EVENTS.to_string()],
    )
    .unwrap_err()
    .contains("downgrade"));

    response.protocol = ABI_VERSION;
    response.binding.session_id = "session-b".to_string();
    if let ExtensionMessage::Ready {
        selected_version, ..
    } = &mut response.message
    {
        *selected_version = ABI_VERSION;
    }
    assert!(
        validate_ready(&request, &response, ABI_VERSION, ABI_VERSION, &[])
            .unwrap_err()
            .contains("binding")
    );
}

#[test]
fn monotonic_deadline_round_trips_and_expires_without_wall_clock() {
    let deadline = MonotonicDeadlineNs::after(Duration::from_millis(20)).unwrap();
    let encoded = serde_json::to_string(&deadline).unwrap();
    let decoded: MonotonicDeadlineNs = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, deadline);
    assert!(decoded.remaining().is_ok());
    std::thread::sleep(Duration::from_millis(25));
    assert!(decoded.remaining().unwrap_err().contains("expired"));
}

#[test]
fn model_attempt_events_carry_identity_usage_and_error_class_only() {
    let pre = EventPayload::PreModelCall {
        turn_index: 3,
        attempt_id: "attempt-a".to_string(),
        provider: "mock".to_string(),
        model: "mock-model".to_string(),
    };
    let post = EventPayload::PostModelCall {
        turn_index: 3,
        attempt_id: "attempt-a".to_string(),
        provider: "mock".to_string(),
        model: "mock-model".to_string(),
        success: false,
        latency_ms: 12,
        input_tokens: 4,
        output_tokens: 0,
        error_class: Some("rate_limited".to_string()),
    };
    let encoded = format!(
        "{}{}",
        serde_json::to_string(&pre).unwrap(),
        serde_json::to_string(&post).unwrap()
    );
    assert!(encoded.contains("attempt-a"));
    for forbidden in ["prompt", "messages", "reasoning", "credential", "secret"] {
        assert!(!encoded.contains(forbidden), "{encoded}");
    }
}

#[test]
fn additive_fields_are_accepted_but_unknown_lifecycle_is_rejected() {
    let mut value = serde_json::to_value(initialize()).unwrap();
    value["future_optional"] = serde_json::json!({"enabled": true});
    let decoded: AbiRequest = serde_json::from_value(value).unwrap();
    assert!(decoded.additive.contains_key("future_optional"));

    let mut invalid = serde_json::to_value(initialize()).unwrap();
    invalid["message"]["lifecycle"] = serde_json::json!("authorize");
    assert!(serde_json::from_value::<AbiRequest>(invalid).is_err());
}

#[tokio::test]
async fn framing_rejects_malformed_and_oversized_frames_before_allocation() {
    let (mut writer, mut reader) = tokio::io::duplex(MAX_ABI_FRAME_BYTES * 2);
    let request = initialize();
    let write = tokio::spawn(async move { write_request(&mut writer, &request).await });
    let decoded = read_request(&mut reader).await.unwrap();
    write.await.unwrap().unwrap();
    assert_eq!(decoded.sequence, 0);

    let (mut writer, mut reader) = tokio::io::duplex(64);
    let malformed = tokio::spawn(async move {
        writer
            .write_all(b"BAD!\x01\x00\x00\x00\x00\x02{}")
            .await
            .unwrap();
    });
    assert!(read_request(&mut reader)
        .await
        .unwrap_err()
        .contains("malformed"));
    malformed.await.unwrap();

    let (mut writer, mut reader) = tokio::io::duplex(64);
    let oversized = tokio::spawn(async move {
        let mut header = [0u8; HEADER_BYTES];
        header[..4].copy_from_slice(&MAGIC);
        header[4] = REQUEST_KIND;
        header[6..].copy_from_slice(&((MAX_ABI_FRAME_BYTES + 1) as u32).to_be_bytes());
        writer.write_all(&header).await.unwrap();
    });
    assert!(read_request(&mut reader)
        .await
        .unwrap_err()
        .contains("exceeds"));
    oversized.await.unwrap();
}

#[test]
fn result_limits_and_correlation_are_enforced() {
    let event_id = "event-a";
    let request = AbiRequest {
        protocol: ABI_VERSION,
        binding: binding(),
        sequence: 1,
        message: HostMessage::Event {
            event_id: event_id.to_string(),
            deadline_monotonic_ns: MonotonicDeadlineNs::after(Duration::from_secs(1)).unwrap(),
            payload: EventPayload::SessionStart {
                source: "broker-task".to_string(),
                attended: false,
                delegated: false,
            },
            capability_refs: Vec::new(),
        },
        additive: BTreeMap::new(),
    };
    let response = AbiResponse {
        protocol: ABI_VERSION,
        binding: binding(),
        sequence: 1,
        message: ExtensionMessage::Result {
            event_id: event_id.to_string(),
            output: Some("ok".to_string()),
            proposed_actions: Vec::new(),
        },
        additive: BTreeMap::new(),
    };
    validate_result(&request, &response, event_id, 2, 0).unwrap();
    assert!(validate_result(&request, &response, "event-b", 2, 0)
        .unwrap_err()
        .contains("correlate"));
    assert!(validate_result(&request, &response, event_id, 1, 0)
        .unwrap_err()
        .contains("limits"));
}
