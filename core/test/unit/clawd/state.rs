use super::*;

#[test]
fn poisoned_context_lock_returns_typed_corruption() {
    let state = DaemonState {
        inner: Arc::new(DaemonStateInner {
            started_at: Utc::now(),
            started_instant: Instant::now(),
            context: Mutex::new(BTreeMap::new()),
            transactions: Mutex::new(BTreeMap::new()),
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
