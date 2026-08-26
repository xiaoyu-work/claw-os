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
