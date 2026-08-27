use super::*;
use crate::protocol::{ClientCapabilities, ContentItem, JsonRpcRequest, RequestId};
use crate::tool::ToolResult;
use crate::transport::in_memory_pair;
use async_trait::async_trait;

struct Echo;

#[async_trait]
impl Tool for Echo {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn description(&self) -> &'static str {
        "echo back"
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
            "additionalProperties": false,
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let t = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
        ToolResult::ok(t.to_string())
    }
}

async fn drive_initialize(t: &impl Transport) -> InitializeResult {
    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: 1.into(),
        method: "initialize".to_string(),
        params: Some(
            serde_json::to_value(&InitializeParams {
                protocol_version: crate::protocol::PROTOCOL_VERSION.to_string(),
                capabilities: ClientCapabilities::default(),
                client_info: Implementation {
                    name: "test".into(),
                    version: "0".into(),
                },
            })
            .unwrap(),
        ),
    };
    t.send(serde_json::to_string(&req).unwrap()).await.unwrap();
    let frame = t.recv().await.unwrap().unwrap();
    let resp: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
    serde_json::from_value(resp.result.unwrap()).unwrap()
}

#[tokio::test]
async fn initialize_advertises_tools_capability() {
    let (client, server) = in_memory_pair();
    let s = Server::new("my-app", "0.1.0").tool(Arc::new(Echo));
    let handle = tokio::spawn(s.serve(server));
    let init = drive_initialize(&client).await;
    assert_eq!(init.server_info.name, "my-app");
    assert_eq!(init.server_info.version, "0.1.0");
    assert!(init.capabilities.tools.is_some());
    drop(client);
    let _ = handle.await;
}

#[tokio::test]
async fn tools_list_returns_registered_tools_sorted() {
    struct A;
    struct B;
    struct C;
    macro_rules! impl_simple_tool {
        ($t:ty, $n:literal) => {
            #[async_trait]
            impl Tool for $t {
                fn name(&self) -> &'static str {
                    $n
                }
                fn description(&self) -> &'static str {
                    $n
                }
                fn input_schema(&self) -> Value {
                    json!({"type":"object"})
                }
                async fn exec(&self, _: Value) -> ToolResult {
                    ToolResult::ok($n)
                }
            }
        };
    }
    impl_simple_tool!(A, "alpha");
    impl_simple_tool!(B, "bravo");
    impl_simple_tool!(C, "charlie");
    // Register in non-sorted order.
    let s = Server::new("t", "0")
        .tool(Arc::new(C))
        .tool(Arc::new(A))
        .tool(Arc::new(B));
    let (client, server) = in_memory_pair();
    let handle = tokio::spawn(s.serve(server));
    let _ = drive_initialize(&client).await;

    let req = JsonRpcRequest::new(2, "tools/list", None);
    client
        .send(serde_json::to_string(&req).unwrap())
        .await
        .unwrap();
    let frame = client.recv().await.unwrap().unwrap();
    let resp: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
    let listing: ListToolsResult = serde_json::from_value(resp.result.unwrap()).unwrap();
    let names: Vec<String> = listing.tools.iter().map(|t| t.name.clone()).collect();
    assert_eq!(names, vec!["alpha", "bravo", "charlie"]);
    drop(client);
    let _ = handle.await;
}

#[tokio::test]
async fn tools_call_routes_to_handler() {
    let s = Server::new("t", "0").tool(Arc::new(Echo));
    let (client, server) = in_memory_pair();
    let handle = tokio::spawn(s.serve(server));
    let _ = drive_initialize(&client).await;

    let req = JsonRpcRequest::new(
        2,
        "tools/call",
        Some(json!({"name":"echo","arguments":{"text":"hi"}})),
    );
    client
        .send(serde_json::to_string(&req).unwrap())
        .await
        .unwrap();
    let frame = client.recv().await.unwrap().unwrap();
    let resp: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
    let result: CallToolResult = serde_json::from_value(resp.result.unwrap()).unwrap();
    assert!(!result.is_error.unwrap_or(false));
    match result.content.first() {
        Some(ContentItem::Text { text }) => assert_eq!(text, "hi"),
        _ => panic!("expected text content"),
    }
    drop(client);
    let _ = handle.await;
}

#[tokio::test]
async fn unknown_tool_yields_invalid_params() {
    let s = Server::new("t", "0").tool(Arc::new(Echo));
    let (client, server) = in_memory_pair();
    let handle = tokio::spawn(s.serve(server));
    let _ = drive_initialize(&client).await;

    let req = JsonRpcRequest::new(
        2,
        "tools/call",
        Some(json!({"name":"missing","arguments":{}})),
    );
    client
        .send(serde_json::to_string(&req).unwrap())
        .await
        .unwrap();
    let frame = client.recv().await.unwrap().unwrap();
    let resp: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
    let err = resp.error.unwrap();
    assert_eq!(err.code, ERR_INVALID_PARAMS);
    drop(client);
    let _ = handle.await;
}

#[tokio::test]
async fn notifications_initialized_is_silently_dropped() {
    let s = Server::new("t", "0").tool(Arc::new(Echo));
    let (client, server) = in_memory_pair();
    let handle = tokio::spawn(s.serve(server));

    // Send a notification then a ping; the server must not have
    // tried to respond to the notification, so the ping response
    // arrives first / unambiguously.
    let note = JsonRpcNotification {
        jsonrpc: "2.0".into(),
        method: "notifications/initialized".into(),
        params: None,
    };
    client
        .send(serde_json::to_string(&note).unwrap())
        .await
        .unwrap();
    let ping = JsonRpcRequest::new(42, "ping", None);
    client
        .send(serde_json::to_string(&ping).unwrap())
        .await
        .unwrap();

    let frame = client.recv().await.unwrap().unwrap();
    let resp: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
    assert_eq!(resp.id, 42.into());
    drop(client);
    let _ = handle.await;
}

#[tokio::test]
async fn parse_error_keeps_server_alive() {
    let s = Server::new("t", "0").tool(Arc::new(Echo));
    let (client, server) = in_memory_pair();
    let handle = tokio::spawn(s.serve(server));

    client.send("not json".into()).await.unwrap();
    let frame = client.recv().await.unwrap().unwrap();
    let resp: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
    assert!(resp.error.is_some());

    // Still alive: ping round-trips.
    let req = JsonRpcRequest::new(2, "ping", None);
    client
        .send(serde_json::to_string(&req).unwrap())
        .await
        .unwrap();
    let frame = client.recv().await.unwrap().unwrap();
    let resp: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
    assert!(resp.error.is_none());
    drop(client);
    let _ = handle.await;
}

#[tokio::test]
async fn valid_json_with_invalid_request_shape_is_not_a_parse_error() {
    let (client, server) = in_memory_pair();
    let handle = tokio::spawn(Server::new("t", "0").serve(server));

    client.send("[]".into()).await.unwrap();
    let frame = client.recv().await.unwrap().unwrap();
    let response: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
    assert_eq!(response.id, RequestId::Null);
    assert_eq!(response.error.unwrap().code, ERR_INVALID_REQUEST);

    drop(client);
    let _ = handle.await;
}

#[tokio::test]
async fn malformed_idless_envelope_is_not_silently_dropped() {
    let (client, server) = in_memory_pair();
    let handle = tokio::spawn(Server::new("t", "0").serve(server));

    client
        .send(r#"{"jsonrpc":"2.0","params":{}}"#.into())
        .await
        .unwrap();
    let frame = client.recv().await.unwrap().unwrap();
    let response: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
    assert_eq!(response.id, RequestId::Null);
    assert_eq!(response.error.unwrap().code, ERR_INVALID_REQUEST);

    drop(client);
    let _ = handle.await;
}

#[tokio::test]
async fn explicit_null_arguments_are_invalid_params() {
    let (client, server) = in_memory_pair();
    let handle = tokio::spawn(Server::new("t", "0").tool(Arc::new(Echo)).serve(server));

    client
        .send(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":null}}"#
                .into(),
        )
        .await
        .unwrap();
    let frame = client.recv().await.unwrap().unwrap();
    let response: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
    assert_eq!(response.error.unwrap().code, ERR_INVALID_PARAMS);

    drop(client);
    let _ = handle.await;
}

#[tokio::test]
async fn primitive_params_are_invalid_before_notification_suppression() {
    let (client, server) = in_memory_pair();
    let handle = tokio::spawn(Server::new("t", "0").serve(server));

    for (frame, expected_id) in [
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":true}"#,
            json!(1),
        ),
        (
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":"bad"}"#,
            json!(2),
        ),
        (
            r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":7}"#,
            Value::Null,
        ),
    ] {
        client.send(frame.into()).await.unwrap();
        let frame = client.recv().await.unwrap().unwrap();
        let response: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(response["id"], expected_id);
        assert_eq!(response["error"]["code"], ERR_INVALID_REQUEST);
    }

    drop(client);
    let _ = handle.await;
}

#[tokio::test]
async fn fractional_and_large_numeric_ids_round_trip() {
    let (client, server) = in_memory_pair();
    let handle = tokio::spawn(Server::new("t", "0").serve(server));

    for request in [
        r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":9223372036854775808,"method":"ping"}"#,
    ] {
        client.send(request.into()).await.unwrap();
        let response = client.recv().await.unwrap().unwrap();
        let request_value: Value = serde_json::from_str(request).unwrap();
        let response_value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response_value["id"], request_value["id"]);
    }

    drop(client);
    let _ = handle.await;
}

#[tokio::test]
async fn initialize_without_params_yields_invalid_params() {
    let (client, server) = in_memory_pair();
    let handle = tokio::spawn(Server::new("t", "0").serve(server));

    let request = JsonRpcRequest::new(1, "initialize", None);
    client
        .send(serde_json::to_string(&request).unwrap())
        .await
        .unwrap();
    let frame = client.recv().await.unwrap().unwrap();
    let response: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
    assert_eq!(response.error.unwrap().code, ERR_INVALID_PARAMS);

    drop(client);
    let _ = handle.await;
}
