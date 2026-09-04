use super::*;

#[test]
fn request_serializes_with_jsonrpc_field() {
    let r = JsonRpcRequest::new(1, "ping", None);
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["id"], 1);
    assert_eq!(v["method"], "ping");
    assert!(v.get("params").is_none(), "params None must be skipped");
}

#[test]
fn notification_has_no_id_field() {
    let n = JsonRpcNotification::new("notifications/cancelled", None);
    let v = serde_json::to_value(&n).unwrap();
    assert!(v.get("id").is_none());
    assert_eq!(v["method"], "notifications/cancelled");
}

#[test]
fn response_ok_omits_error() {
    let r = JsonRpcResponse::ok(7.into(), serde_json::json!({"ok": true}));
    let v = serde_json::to_value(&r).unwrap();
    assert!(v.get("error").is_none());
    assert_eq!(v["result"]["ok"], true);
}

#[test]
fn response_err_omits_result() {
    let r = JsonRpcResponse::err(
        RequestId::Str("a".into()),
        JsonRpcError::new(ERR_METHOD_NOT_FOUND, "no such method"),
    );
    let v = serde_json::to_value(&r).unwrap();
    assert!(v.get("result").is_none());
    assert_eq!(v["error"]["code"], ERR_METHOD_NOT_FOUND);
    assert_eq!(v["error"]["message"], "no such method");
}

#[test]
fn request_id_round_trips_both_kinds() {
    for input in [
        "{\"jsonrpc\":\"2.0\",\"id\":42,\"method\":\"x\"}",
        "{\"jsonrpc\":\"2.0\",\"id\":\"abc\",\"method\":\"x\"}",
    ] {
        let r: JsonRpcRequest = serde_json::from_str(input).unwrap();
        let again = serde_json::to_string(&r).unwrap();
        // Parse again to confirm round-trip is stable.
        let _: JsonRpcRequest = serde_json::from_str(&again).unwrap();
    }
}

#[test]
fn initialize_result_has_required_fields() {
    let body = serde_json::json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {"tools": {"listChanged": true}},
        "serverInfo": {"name": "cos", "version": "0.1.0"}
    });
    let init: InitializeResult = serde_json::from_value(body).unwrap();
    assert_eq!(init.protocol_version, "2025-06-18");
    assert_eq!(init.server_info.name, "cos");
    assert!(init.capabilities.tools.is_some());
}

#[test]
fn call_tool_result_is_error_round_trip() {
    let r = CallToolResult {
        content: vec![ContentItem::Text {
            text: "boom".into(),
        }],
        is_error: Some(true),
        structured_content: None,
    };
    let s = serde_json::to_string(&r).unwrap();
    assert!(s.contains("\"isError\":true"));
    let back: CallToolResult = serde_json::from_str(&s).unwrap();
    assert_eq!(back.is_error, Some(true));
}

#[test]
fn tool_descriptor_serializes_input_schema_camel_case() {
    let t = ToolDescriptor {
        name: "echo".into(),
        description: Some("echo".into()),
        input_schema: serde_json::json!({"type":"object"}),
    };
    let s = serde_json::to_string(&t).unwrap();
    assert!(s.contains("\"inputSchema\""));
}
