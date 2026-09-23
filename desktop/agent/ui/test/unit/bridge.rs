use super::*;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn endpoint(minimum: u16, current: u16) -> BridgeEndpoint {
    BridgeEndpoint {
        port: 43123,
        token: TOKEN.into(),
        min_protocol_version: ProtocolVersion(minimum),
        protocol_version: ProtocolVersion(current),
    }
}

#[test]
fn legacy_discovery_fixture_requires_one_restart_cycle() {
    let state = decode_bridge_discovery(
        br#"{"port":43123,"token":"0123456789abcdef0123456789abcdef"}"#,
    )
    .unwrap();
    assert!(matches!(state, DiscoveryState::UpgradeRequired));
    assert_eq!(
        service_action(&state, HealthStatus::NegotiationFailed),
        Some(ServiceAction::Restart)
    );
}

#[test]
fn old_health_fixture_without_echo_requires_restart() {
    let selected = ProtocolVersion(1);
    let old_health_headers = HeaderMap::new();
    assert!(validate_response_protocol_headers(&old_health_headers, selected).is_err());

    let state = DiscoveryState::Ready(endpoint(1, 1));
    assert_eq!(
        service_action(&state, HealthStatus::NegotiationFailed),
        Some(ServiceAction::Restart)
    );
}

#[test]
fn future_bridge_negotiates_highest_client_overlap() {
    let state = decode_bridge_discovery(
        br#"{"port":43123,"token":"0123456789abcdef0123456789abcdef","protocol_version":4,"min_protocol_version":3}"#,
    )
    .unwrap();
    let DiscoveryState::Ready(future) = state else {
        panic!("future bridge with a v3 overlap must be usable");
    };
    assert_eq!(
        selected_protocol_version(&future).unwrap(),
        ProtocolVersion(3)
    );

    let mut echoed = HeaderMap::new();
    echoed.insert(PROTOCOL_VERSION_HEADER, "3".parse().unwrap());
    assert!(validate_response_protocol_headers(&echoed, ProtocolVersion(3)).is_ok());
    assert!(validate_response_protocol_headers(&echoed, ProtocolVersion(1)).is_err());
}

#[test]
fn discovery_without_overlap_requires_upgrade() {
    let state = decode_bridge_discovery(
        br#"{"port":43123,"token":"0123456789abcdef0123456789abcdef","protocol_version":1,"min_protocol_version":1}"#,
    )
    .unwrap();
    assert!(matches!(state, DiscoveryState::UpgradeRequired));
    assert_eq!(
        service_action(&state, HealthStatus::NegotiationFailed),
        Some(ServiceAction::Restart)
    );
    assert!(selected_protocol_version(&endpoint(1, 1)).is_err());
}

#[test]
fn healthy_manual_bridge_is_left_running() {
    let state = DiscoveryState::Ready(endpoint(3, 3));
    assert_eq!(service_action(&state, HealthStatus::Healthy), None);
    assert_eq!(
        service_action(&state, HealthStatus::Unavailable),
        Some(ServiceAction::Start)
    );
}

#[test]
fn upgrade_restart_can_only_be_claimed_once() {
    let attempted = AtomicBool::new(false);
    assert!(claim_upgrade_restart(&attempted));
    assert!(!claim_upgrade_restart(&attempted));
}

#[test]
fn activity_requests_authenticate_negotiate_and_escape_resource_identity() {
    let endpoint = endpoint(3, 3);
    let (request, selected) = activity_request(
        &endpoint,
        reqwest::Method::POST,
        &["activities", "id/with?query#fragment", "run"],
    ).unwrap();
    let request = request.json(&ActivityRunRequest {
        prompt: Some("Continue".into()),
        session_id: Some("session-1".into()),
        ..ActivityRunRequest::default()
    }).build().unwrap();
    assert_eq!(selected, ProtocolVersion(3));
    assert_eq!(request.headers()[PROTOCOL_VERSION_HEADER], "3");
    assert_eq!(request.headers()["authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(request.url().host_str(), Some("127.0.0.1"));
    assert_eq!(request.url().path_segments().unwrap().count(), 4);
    assert!(request.url().query().is_none());
    assert!(request.url().fragment().is_none());
    let body: ActivityRunRequest =
        serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
    assert_eq!(body.session_id.as_deref(), Some("session-1"));
}

#[test]
fn activity_object_transport_uses_the_versioned_endpoint_and_typed_components() {
    let endpoint = endpoint(3, 3);
    let body = ActivityObjectAttachRequest {
        label: "Release status".into(),
        object: cos_agent_protocol::AppObjectReference {
            app_id: "kv".into(),
            object_type: "entry".into(),
            object_id: " a/b?x=1&y=2 ".into(),
            revision: Some("v1/#? ".into()),
        },
    };
    let (request, selected) = activity_request(
        &endpoint,
        reqwest::Method::POST,
        &["activities", "activity-1", "objects"],
    ).unwrap();
    let request = request.json(&body).build().unwrap();
    assert_eq!(selected, ProtocolVersion(3));
    assert_eq!(request.headers()[PROTOCOL_VERSION_HEADER], "3");
    assert!(request.headers().contains_key(reqwest::header::AUTHORIZATION));
    assert_eq!(request.url().path(), "/api/activities/activity-1/objects");
    let sent: ActivityObjectAttachRequest =
        serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
    assert_eq!(sent, body);
}

#[test]
fn activity_continuity_transport_is_authenticated_versioned_and_owner_free() {
    let endpoint = endpoint(3, 3);
    let (export, selected) =
        continuity_export_request(&endpoint, "activity/id?not-query").unwrap();
    let export = export.build().unwrap();
    assert_eq!(selected, ProtocolVersion(3));
    assert_eq!(export.method(), reqwest::Method::GET);
    assert!(export.headers().contains_key(reqwest::header::AUTHORIZATION));
    assert_eq!(export.headers()[PROTOCOL_VERSION_HEADER], "3");
    assert_eq!(export.url().path_segments().unwrap().count(), 5);
    assert!(export.url().path().ends_with("/continuity/export"));
    assert!(export.url().query().is_none());
    assert!(export.body().is_none());

    let document: ActivityContinuityDocument = serde_json::from_value(serde_json::json!({
        "kind": "claw_os.activity_continuity",
        "schema_version": 1,
        "lineage": {"id": "00000000-0000-4000-8000-000000000123", "revision": 7},
        "snapshot": "sha256:6d8c822b0519a02e7ab1ebeab48392cb83756fa9cc8b6d39e94b01ab1682882b",
        "intent": {
            "title": "Release", "goal": "Publish the release",
            "completion_criteria": "Reviewed and available",
            "boundaries": "Ask before publishing"
        },
        "references": [{
            "label": "Status",
            "reference": "app://kv/entry?id=release.status&revision=v1"
        }],
        "rules": {
            "execution_limits": {
                "enabled": false, "max_attempts": 10, "max_turns_per_attempt": 5,
                "expires_at": "2030-01-01T00:00:00.000000000Z"
            },
            "scheduling": {"priority": "foreground"}
        }
    }))
    .unwrap();
    let body = ActivityContinuityImportRequest {
        placement: cos_agent_protocol::ActivityExecutionPlacement::Local,
        document: document.to_json().unwrap(),
    };
    let (import, selected) = continuity_import_request(&endpoint, &body).unwrap();
    let import = import.build().unwrap();
    assert_eq!(selected, ProtocolVersion(3));
    assert_eq!(import.method(), reqwest::Method::POST);
    assert_eq!(import.url().path(), "/api/activities/continuity/import");
    assert!(import.url().query().is_none());
    assert!(import.headers().contains_key(reqwest::header::AUTHORIZATION));
    let sent: serde_json::Value =
        serde_json::from_slice(import.body().unwrap().as_bytes().unwrap()).unwrap();
    assert_eq!(sent.as_object().unwrap().len(), 2);
    assert_eq!(sent["placement"], "local");
    assert_eq!(sent["document"], body.document);
    for field in ["owner_uid", "authority", "path", "restore", "live_sync"] {
        assert!(sent.get(field).is_none(), "{field}");
    }
}

#[test]
fn activity_operation_preview_transport_is_authenticated_versioned_and_typed() {
    let endpoint = endpoint(3, 3);
    let body = ActivityOperationPreviewRequest {
        app_id: "kv".into(), operation: "get".into(),
        args: vec!["entry; $(not executed)".into()],
    };
    let (request, selected) = activity_request(
        &endpoint, reqwest::Method::POST, &["activities", "activity-1", "operation-preview"],
    ).unwrap();
    let request = request.json(&body).build().unwrap();
    assert_eq!(selected, ProtocolVersion(3));
    assert_eq!(request.headers()[PROTOCOL_VERSION_HEADER], "3");
    assert!(request.headers().contains_key(reqwest::header::AUTHORIZATION));
    assert_eq!(request.url().path(), "/api/activities/activity-1/operation-preview");
    assert_eq!(request.method(), reqwest::Method::POST);
    assert_eq!(
        serde_json::from_slice::<ActivityOperationPreviewRequest>(
            request.body().unwrap().as_bytes().unwrap()
        ).unwrap(),
        body,
    );
}

#[test]
fn activity_receipts_transport_is_authenticated_versioned_read_only_and_bounded() {
    let endpoint = endpoint(3, 3);
    let (request, selected) = activity_request(
        &endpoint, reqwest::Method::GET, &["activities", "activity-1", "receipts"],
    ).unwrap();
    let request = request.query(&ActivityReceiptsQuery { limit: Some(100) }).build().unwrap();
    assert_eq!(selected, ProtocolVersion(3));
    assert_eq!(request.headers()[PROTOCOL_VERSION_HEADER], "3");
    assert!(request.headers().contains_key(reqwest::header::AUTHORIZATION));
    assert_eq!(request.method(), reqwest::Method::GET);
    assert_eq!(request.url().path(), "/api/activities/activity-1/receipts");
    assert_eq!(request.url().query(), Some("limit=100"));
    assert!(request.body().is_none());
}

#[test]
fn activity_object_state_list_transport_preserves_exact_reference_and_bounds() {
    let endpoint = endpoint(3, 3);
    let reference = "app://kv/entry?id=a%2Fb%3Fx%3D1%26y%3D2&rev=v%2F1";
    let (request, selected) = object_state_list_request(
        &endpoint, "activity/id?not-query",
        &ActivityObjectStateQuery { reference: Some(reference.into()), limit: Some(100) },
    ).unwrap();
    let request = request.build().unwrap();
    assert_eq!(selected, ProtocolVersion(3));
    assert_eq!(request.method(), reqwest::Method::GET);
    assert_eq!(request.headers()[PROTOCOL_VERSION_HEADER], "3");
    assert!(request.headers().contains_key(reqwest::header::AUTHORIZATION));
    assert_eq!(request.url().path_segments().unwrap().count(), 4);
    assert!(request.url().path().ends_with("/object-state"));
    assert!(request.url().fragment().is_none());
    let query: std::collections::HashMap<_, _> = request.url().query_pairs().into_owned().collect();
    assert_eq!(query["reference"], reference);
    assert_eq!(query["limit"], "100");
    assert_eq!(query.len(), 2);
    assert!(request.body().is_none());
    let (request, _) = object_state_list_request(
        &endpoint, "activity", &ActivityObjectStateQuery::default(),
    ).unwrap();
    assert!(request.build().unwrap().url().query().is_none_or(str::is_empty));
}

#[test]
fn activity_object_state_record_transport_reuses_uuid_and_serializes_only_the_draft() {
    use cos_agent_protocol::{ObjectStateContent, ObjectStateDraft};
    let endpoint = endpoint(3, 3);
    let body = ActivityObjectStateRecordRequest {
        entry: ObjectStateDraft {
            id: "22222222-2222-4222-8222-222222222222".into(),
            reference: "app://kv/entry?id=a%2Fb%3Fx%3D1".into(),
            content: ObjectStateContent::Retracted { reason: "Not a verified observation".into() },
            observed_at: None, valid_until: None,
            supersedes: Some("33333333-3333-4333-8333-333333333333".into()),
        },
    };
    let mut encoded = Vec::new();
    for _ in 0..2 {
        let (request, selected) = object_state_record_request(&endpoint, "activity", &body).unwrap();
        let request = request.build().unwrap();
        assert_eq!(selected, ProtocolVersion(3));
        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(request.url().path(), "/api/activities/activity/object-state");
        assert_eq!(request.headers()[PROTOCOL_VERSION_HEADER], "3");
        assert!(request.headers().contains_key(reqwest::header::AUTHORIZATION));
        assert!(request.url().query().is_none());
        let bytes = request.body().unwrap().as_bytes().unwrap();
        assert_eq!(serde_json::from_slice::<ActivityObjectStateRecordRequest>(bytes).unwrap(), body);
        encoded.push(bytes.to_vec());
    }
    assert_eq!(encoded[0], encoded[1]);
    let sent: serde_json::Value = serde_json::from_slice(&encoded[0]).unwrap();
    assert_eq!(sent.as_object().unwrap().len(), 1);
    assert_eq!(sent["entry"].as_object().unwrap().len(), 6);
    assert_eq!(sent["entry"]["content"]["kind"], "retracted");
    for field in ["owner_uid", "activity_id", "source", "receipt", "validity", "execute"] {
        assert!(sent.get(field).is_none());
        assert!(sent["entry"].get(field).is_none());
    }
}

#[test]
fn activity_execution_limits_transport_is_authenticated_versioned_and_keeps_activity_only_in_path() {
    use cos_agent_protocol::ExecutionLimitsDraft;
    let endpoint = endpoint(3, 3);
    let id = "activity/id?not-a-query";
    let (get, selected) = execution_limits_get_request(&endpoint, id).unwrap();
    let get = get.build().unwrap();
    assert_eq!(selected, ProtocolVersion(3));
    assert_eq!(get.method(), reqwest::Method::GET);
    assert_eq!(get.url().path_segments().unwrap().count(), 4);
    assert!(get.url().path().ends_with("/execution-limits"));
    assert!(get.url().query().is_none());
    assert!(get.body().is_none());
    assert!(get.headers().contains_key(reqwest::header::AUTHORIZATION));
    assert_eq!(get.headers()[PROTOCOL_VERSION_HEADER], "3");

    let body = ActivityExecutionLimitsSetRequest {
        expected_revision: None,
        limits: ExecutionLimitsDraft {
            max_attempts: 5,
            max_turns_per_attempt: 20,
            expires_at: "2099-01-01T05:00:00-07:00".into(),
        },
    };
    let (set, _) = execution_limits_set_request(&endpoint, id, &body).unwrap();
    let set = set.build().unwrap();
    assert_eq!(set.method(), reqwest::Method::POST);
    assert_eq!(set.url(), get.url());
    assert_eq!(set.headers()[PROTOCOL_VERSION_HEADER], "3");
    assert!(set.headers().contains_key(reqwest::header::AUTHORIZATION));
    let value: serde_json::Value =
        serde_json::from_slice(set.body().unwrap().as_bytes().unwrap()).unwrap();
    assert_eq!(value, serde_json::json!({
        "expected_revision": null,
        "limits": {"max_attempts": 5, "max_turns_per_attempt": 20, "expires_at": "2099-01-01T05:00:00-07:00"}
    }));
    assert!(value.get("id").is_none());
    assert!(value.get("owner_uid").is_none());
    assert!(value.get("used_attempts").is_none());
}

#[test]
fn activity_execution_limits_toggle_transport_preserves_large_cas_and_has_no_reset_fields() {
    let endpoint = endpoint(3, 3);
    let body = ActivityExecutionLimitsEnabledRequest {
        expected_revision: u64::MAX - 1,
        enabled: false,
    };
    let (request, selected) =
        execution_limits_enabled_request(&endpoint, "activity", &body).unwrap();
    let request = request.build().unwrap();
    assert_eq!(selected, ProtocolVersion(3));
    assert_eq!(request.method(), reqwest::Method::POST);
    assert_eq!(request.url().path(), "/api/activities/activity/execution-limits/enabled");
    assert!(request.url().query().is_none());
    assert_eq!(request.headers()[PROTOCOL_VERSION_HEADER], "3");
    assert!(request.headers().contains_key(reqwest::header::AUTHORIZATION));
    let encoded = request.body().unwrap().as_bytes().unwrap();
    assert_eq!(
        serde_json::from_slice::<ActivityExecutionLimitsEnabledRequest>(encoded).unwrap(),
        body,
    );
    let value: serde_json::Value = serde_json::from_slice(encoded).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 2);
    assert_eq!(value["expected_revision"].as_u64(), Some(u64::MAX - 1));
    assert_eq!(value["enabled"], false);
}

#[test]
fn activity_monetary_budget_transport_is_authenticated_versioned_and_preserves_u64_cas() {
    use cos_agent_protocol::{MonetaryBudgetDraft, MonetaryCurrency};
    let endpoint = endpoint(3, 3);
    let id = "activity/id?not-a-query";
    let (get, selected) = monetary_budget_get_request(&endpoint, id).unwrap();
    assert_eq!(selected, ProtocolVersion(3));
    let get = get.build().unwrap();
    assert_eq!(
        get.url().path(),
        "/api/activities/activity%2Fid%3Fnot-a-query/monetary-budget"
    );
    assert!(get.headers().contains_key(reqwest::header::AUTHORIZATION));
    let body = ActivityMonetaryBudgetSetRequest {
        expected_revision: Some(u64::MAX - 1),
        budget: MonetaryBudgetDraft {
            currency: MonetaryCurrency::Usd,
            max_total_microusd: 5_000_000,
            input_microusd_per_million_tokens: 250_000,
            output_microusd_per_million_tokens: 1_000_000,
            max_output_tokens_per_turn: 4096,
        },
    };
    let (request, _) = monetary_budget_set_request(&endpoint, id, &body).unwrap();
    let request = request.build().unwrap();
    let encoded = request.body().unwrap().as_bytes().unwrap();
    assert_eq!(
        serde_json::from_slice::<ActivityMonetaryBudgetSetRequest>(encoded).unwrap(),
        body
    );
    let value: serde_json::Value = serde_json::from_slice(encoded).unwrap();
    assert!(value.get("owner_uid").is_none());
    assert!(value.get("spent_microusd").is_none());
    let toggle = ActivityMonetaryBudgetEnabledRequest {
        expected_revision: u64::MAX - 1,
        enabled: false,
    };
    let (request, _) = monetary_budget_enabled_request(&endpoint, id, &toggle).unwrap();
    let request = request.build().unwrap();
    assert_eq!(
        serde_json::from_slice::<ActivityMonetaryBudgetEnabledRequest>(
            request.body().unwrap().as_bytes().unwrap()
        )
        .unwrap(),
        toggle
    );
}

#[test]
fn activity_scheduling_priority_transport_is_authenticated_closed_and_preserves_u64_cas() {
    let endpoint = endpoint(3, 3);
    let id = "activity/id?not-a-query";
    let (get, selected) = scheduling_priority_get_request(&endpoint, id).unwrap();
    assert_eq!(selected, ProtocolVersion(3));
    let get = get.build().unwrap();
    assert_eq!(
        get.url().path(),
        "/api/activities/activity%2Fid%3Fnot-a-query/scheduling-priority"
    );
    assert!(get.url().query().is_none());
    assert!(get.headers().contains_key(reqwest::header::AUTHORIZATION));
    let body = ActivitySchedulingPrioritySetRequest {
        expected_revision: Some(u64::MAX - 1),
        priority: ActivitySchedulingPriority::Background,
    };
    let (request, _) = scheduling_priority_set_request(&endpoint, id, &body).unwrap();
    let request = request.build().unwrap();
    let encoded = request.body().unwrap().as_bytes().unwrap();
    assert_eq!(
        serde_json::from_slice::<ActivitySchedulingPrioritySetRequest>(encoded).unwrap(),
        body
    );
    let value: serde_json::Value = serde_json::from_slice(encoded).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 2);
    for field in ["owner_uid", "activity_id", "job_id", "authority", "preempt"] {
        assert!(value.get(field).is_none(), "{field}");
    }
}
