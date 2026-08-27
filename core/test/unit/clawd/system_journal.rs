use super::*;

#[test]
fn query_returns_recent_operations() {
    let tmp = tempfile::tempdir().unwrap();
    let prev = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_DATA_DIR", tmp.path());

    let client = ClientIdentity::unknown();
    let response = Response::ok(None, json!({"status": "ok"}));
    record_clawd_request(
        "daemon.health",
        &Value::Null,
        &response,
        Duration::from_millis(3),
        &client,
    );
    let result = query(json!({"limit": 10})).unwrap();
    assert_eq!(result["operations"][0]["source"], "clawd.request");
    assert_eq!(result["operations"][0]["operation"], "daemon.health");

    match prev {
        Some(value) => std::env::set_var("COS_DATA_DIR", value),
        None => std::env::remove_var("COS_DATA_DIR"),
    }
}

#[test]
fn launch_handles_are_masked_in_the_system_journal() {
    let client = ClientIdentity::unknown();
    let response = Response::ok(None, json!({"bound": true}));
    let record = clawd_request_record(
        "app_session.bind",
        &json!({"session_id": "app-1", "handle": "d34db33f", "pid": 4242}),
        &response,
        Duration::from_millis(1),
        &client,
    );
    assert_eq!(record["params"]["handle"], json!("<redacted>"));
    assert_eq!(record["params"]["session_id"], json!("app-1"));
    assert!(
        !serde_json::to_string(&record).unwrap().contains("d34db33f"),
        "the journal projection must not carry replayable launch authority"
    );
}

#[test]
fn peer_only_denial_data_never_reaches_the_system_journal() {
    let client = ClientIdentity::unknown();
    let response = Response::error_with_data(
        None,
        "request_failed",
        crate::clawd::protocol::BrokerError::with_data(
            "launcher cannot delegate sys.identity:name:accounts; awaiting approval",
            json!({"status": "approval_required", "approval_requests": ["ap-1"]}),
        ),
    );
    let record = clawd_request_record(
        "app_session.register",
        &json!({"app_id": "user-manager", "handle": "deadbeef"}),
        &response,
        Duration::from_millis(1),
        &client,
    );
    let rendered = serde_json::to_string(&record).expect("serialize");
    assert_eq!(record["params"]["handle"], json!("<redacted>"));
    assert!(
        !rendered.contains("deadbeef") && !rendered.contains("approval_requests"),
        "neither the launch handle nor peer-only denial data may be journalled"
    );
}
