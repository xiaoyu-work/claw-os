use serde_json::json;

use super::*;

#[test]
fn request_defaults_params_to_null() {
    let request: Request = serde_json::from_value(json!({
        "id": 1,
        "command": "daemon.health"
    }))
    .unwrap();

    assert_eq!(request.id, Some(json!(1)));
    assert_eq!(request.params, Value::Null);
}

#[test]
fn response_error_omits_result() {
    let response = Response::error(Some(json!("r1")), "bad_request", "missing command");
    let value = serde_json::to_value(response).unwrap();

    assert_eq!(value["ok"], false);
    assert!(value.get("result").is_none());
    assert_eq!(value["error"]["code"], "bad_request");
}
