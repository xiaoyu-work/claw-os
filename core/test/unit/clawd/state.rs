use super::*;

#[test]
fn poisoned_context_lock_returns_typed_corruption() {
    let state = DaemonState {
        inner: Arc::new(DaemonStateInner {
            started_at: Utc::now(),
            started_instant: Instant::now(),
            context: Mutex::new(BTreeMap::new()),
            transactions: Mutex::new(BTreeMap::new()),
            app_authorizations: Mutex::new(HashMap::new()),
            app_service_manager: OnceLock::new(),
        }),
    };
    state.poison_context_for_test();

    let error = state.context_snapshot().unwrap_err();
    assert_eq!(error.kind(), StateErrorKind::Corrupt);
    assert_eq!(error.operation(), "state.lock");

    let response = crate::clawd::protocol::Response::handler_error(
        crate::clawd::protocol::RequestId::unknown(),
        error.into(),
    );
    assert_eq!(response.error.unwrap().code, "unavailable");
}

#[test]
fn invalid_transaction_timestamp_is_corrupt_and_maps_to_unavailable() {
    let id = SessionId::generate();
    let error = StateError::parsed_time(&id, "not-a-timestamp").unwrap_err();

    assert_eq!(error.kind(), StateErrorKind::Corrupt);
    assert!(std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<chrono::ParseError>())
        .is_some());
    let response = crate::clawd::protocol::Response::handler_error(
        crate::clawd::protocol::RequestId::unknown(),
        error.into(),
    );
    assert_eq!(response.error.unwrap().code, "unavailable");
}

#[test]
fn session_decode_and_not_found_keep_distinct_state_categories() {
    let decode = serde_json::from_str::<serde_json::Value>("{bad").unwrap_err();
    let corrupt = StateError::session(
        "transaction.recover",
        "read metadata",
        session::SessionError::Decode {
            path: "meta.json".into(),
            source: decode,
        },
    );
    assert_eq!(corrupt.kind(), StateErrorKind::Corrupt);
    assert!(std::error::Error::source(&corrupt)
        .and_then(|source| source.downcast_ref::<session::SessionError>())
        .is_some());

    let missing = StateError::session(
        "transaction.recover",
        "read metadata",
        session::SessionError::NotFound("missing".to_string()),
    );
    assert_eq!(missing.kind(), StateErrorKind::NotFound);
    let response = crate::clawd::protocol::Response::handler_error(
        crate::clawd::protocol::RequestId::unknown(),
        missing.into(),
    );
    assert_eq!(response.error.unwrap().code, "execution_failed");
}

fn app_authorization(expires_at_ms: u64) -> super::super::app_services::AppCallAuthorization {
    super::super::app_services::AppCallAuthorization {
        owner_uid: 1000,
        app_id: "mail".to_string(),
        tool: "messages.list".to_string(),
        caps: crate::caps::CapSet::new(),
        context: crate::agent::tools::app_gateway::McpCallContext {
            wire_version: crate::agent::tools::app_gateway::CALL_CONTEXT_WIRE_VERSION,
            call_id: "call-a".to_string(),
            trace_id: "trace-a".to_string(),
            deadline_unix_ms: Some(crate::agentd::grant::now_ms() + 60_000),
            session_id: Some("session-a".to_string()),
            task_id: Some("task-a".to_string()),
            caller: crate::agent::tools::app_gateway::McpPrincipal {
                kind: crate::agent::tools::app_gateway::McpPrincipalKind::SystemAgent,
                id: "session-a".to_string(),
                owner_uid: 1000,
            },
        },
        capability_generation: "a".repeat(16),
        package: crate::provenance::runtime::PackageRef {
            kind: crate::provenance::PackageKind::App,
            id: "mail".to_string(),
            content_digest: "b".repeat(64),
            publisher_key_id: None,
            tier: "system".to_string(),
        },
        service_host_session_id: "app-service-a".to_string(),
        service_host_pid: 123,
        service_host_start_time_ticks: Some(456),
        service_extension_uid: 61_056,
        action_digest: "c".repeat(64),
        expires_at_ms,
    }
}

#[test]
fn app_authorizations_are_random_single_use_and_expiring() {
    let state = DaemonState {
        inner: Arc::new(DaemonStateInner {
            started_at: Utc::now(),
            started_instant: Instant::now(),
            context: Mutex::new(BTreeMap::new()),
            transactions: Mutex::new(BTreeMap::new()),
            app_authorizations: Mutex::new(HashMap::new()),
            app_service_manager: OnceLock::new(),
        }),
    };
    let token = state
        .issue_app_authorization(app_authorization(crate::agentd::grant::now_ms() + 60_000))
        .expect("issue authorization");
    assert_eq!(token.len(), 32);
    assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let consumed = state
        .consume_app_authorization(&token, &"c".repeat(64))
        .expect("consume authorization once");
    assert_eq!(consumed.app_id, "mail");
    assert!(state
        .consume_app_authorization(&token, &"c".repeat(64))
        .is_err());

    let expired = state
        .issue_app_authorization(app_authorization(
            crate::agentd::grant::now_ms().saturating_sub(1),
        ))
        .expect("issue expired authorization");
    assert!(state
        .consume_app_authorization(&expired, &"c".repeat(64))
        .unwrap_err()
        .contains("expired"));
    assert!(state
        .consume_app_authorization("not-a-token", &"c".repeat(64))
        .is_err());

    let substituted = state
        .issue_app_authorization(app_authorization(crate::agentd::grant::now_ms() + 60_000))
        .expect("issue bound authorization");
    assert!(state
        .consume_app_authorization(&substituted, &"d".repeat(64))
        .unwrap_err()
        .contains("requested action"));
    assert!(
        state
            .consume_app_authorization(&substituted, &"c".repeat(64))
            .is_err(),
        "a substitution attempt must burn the single-use authorization"
    );
}
