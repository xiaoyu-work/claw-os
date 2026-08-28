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
