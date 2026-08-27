use super::super::client::{ClientError, McpClient};
use super::super::protocol::{ClientCapabilities, ContentItem, Implementation};
use super::super::transport::in_memory_pair;
use super::*;
use crate::agent::tools::builtin::Echo;

fn registry_with_echo() -> Arc<ToolRegistry> {
    let mut r = ToolRegistry::new();
    r.register(Arc::new(Echo));
    Arc::new(r)
}

#[tokio::test]
async fn initialize_returns_tools_capability() {
    let (client_t, server_t) = in_memory_pair();
    let server = McpServer::new("cos-test", "0.0.1", registry_with_echo());
    let server_handle = tokio::spawn(server.serve(server_t));

    let client = McpClient::new(client_t);
    client.start().await;
    let init = client
        .initialize(
            Implementation {
                name: "test-client".into(),
                version: "0.0.1".into(),
            },
            ClientCapabilities::default(),
        )
        .await
        .unwrap();
    assert_eq!(init.protocol_version, PROTOCOL_VERSION);
    assert_eq!(init.server_info.name, "cos-test");
    assert!(init.capabilities.tools.is_some());
    drop(client);
    let _ = server_handle.await;
}

#[tokio::test]
async fn tools_list_reflects_registry() {
    let (client_t, server_t) = in_memory_pair();
    let server = McpServer::new("cos", "0", registry_with_echo());
    let server_handle = tokio::spawn(server.serve(server_t));

    let client = McpClient::new(client_t);
    client.start().await;
    let listing = client.list_tools().await.unwrap();
    assert!(listing.tools.iter().any(|t| t.name == "echo"));
    drop(client);
    let _ = server_handle.await;
}

#[tokio::test]
async fn tools_call_executes_registered_tool() {
    let (client_t, server_t) = in_memory_pair();
    let server = McpServer::new("cos", "0", registry_with_echo());
    let server_handle = tokio::spawn(server.serve(server_t));

    let client = McpClient::new(client_t);
    client.start().await;
    let result = client
        .call_tool("echo", Some(serde_json::json!({"text": "hi"})))
        .await
        .unwrap();
    assert!(result.is_error.unwrap_or(false) == false);
    match result.content.first() {
        Some(ContentItem::Text { text }) => assert!(text.contains("hi")),
        _ => panic!("expected text content"),
    }
    drop(client);
    let _ = server_handle.await;
}

#[tokio::test]
async fn unknown_method_yields_method_not_found() {
    let (client_t, server_t) = in_memory_pair();
    let server = McpServer::new("cos", "0", registry_with_echo());
    let server_handle = tokio::spawn(server.serve(server_t));

    let client = McpClient::new(client_t);
    client.start().await;
    let err = client.request("not/a/method", None).await.unwrap_err();
    match err {
        ClientError::Server { code, .. } => {
            assert_eq!(code, ERR_METHOD_NOT_FOUND);
        }
        other => panic!("expected Server error, got {other:?}"),
    }
    drop(client);
    let _ = server_handle.await;
}

#[tokio::test]
async fn unknown_tool_yields_invalid_params() {
    let (client_t, server_t) = in_memory_pair();
    let server = McpServer::new("cos", "0", registry_with_echo());
    let server_handle = tokio::spawn(server.serve(server_t));

    let client = McpClient::new(client_t);
    client.start().await;
    let err = client.call_tool("missing", None).await.unwrap_err();
    match err {
        ClientError::Server { code, .. } => {
            assert_eq!(code, ERR_INVALID_PARAMS);
        }
        other => panic!("expected Server error, got {other:?}"),
    }
    drop(client);
    let _ = server_handle.await;
}

#[tokio::test]
async fn parse_error_does_not_kill_server() {
    let (client_t, server_t) = in_memory_pair();
    let server = McpServer::new("cos", "0", registry_with_echo());
    let server_handle = tokio::spawn(server.serve(server_t));

    // Send a junk frame.
    client_t.send("not json at all".into()).await.unwrap();
    // Read the parse-error response.
    let resp = client_t.recv().await.unwrap().unwrap();
    let parsed: JsonRpcResponse = serde_json::from_str(&resp).unwrap();
    assert!(parsed.error.is_some());

    // Now drive a real request — server must still be alive.
    let client = McpClient::new(client_t);
    client.start().await;
    let _ = client.request("ping", None).await.unwrap();
    drop(client);
    let _ = server_handle.await;
}

#[tokio::test]
async fn valid_json_with_invalid_request_shape_is_not_a_parse_error() {
    let (client_t, server_t) = in_memory_pair();
    let server = McpServer::new("cos", "0", registry_with_echo());
    let server_handle = tokio::spawn(server.serve(server_t));

    client_t.send("[]".into()).await.unwrap();
    let frame = client_t.recv().await.unwrap().unwrap();
    let response: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
    assert_eq!(response.id, super::super::protocol::RequestId::Null);
    assert_eq!(response.error.unwrap().code, ERR_INVALID_REQUEST);

    drop(client_t);
    let _ = server_handle.await;
}

#[tokio::test]
async fn malformed_idless_envelope_is_not_silently_dropped() {
    let (client_t, server_t) = in_memory_pair();
    let server = McpServer::new("cos", "0", registry_with_echo());
    let server_handle = tokio::spawn(server.serve(server_t));

    client_t
        .send(r#"{"jsonrpc":"2.0","params":{}}"#.into())
        .await
        .unwrap();
    let frame = client_t.recv().await.unwrap().unwrap();
    let response: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
    assert_eq!(response.id, super::super::protocol::RequestId::Null);
    assert_eq!(response.error.unwrap().code, ERR_INVALID_REQUEST);

    drop(client_t);
    let _ = server_handle.await;
}

#[tokio::test]
async fn explicit_null_arguments_are_invalid_params() {
    let (client_t, server_t) = in_memory_pair();
    let server = McpServer::new("cos", "0", registry_with_echo());
    let server_handle = tokio::spawn(server.serve(server_t));

    client_t
        .send(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":null}}"#
                .into(),
        )
        .await
        .unwrap();
    let frame = client_t.recv().await.unwrap().unwrap();
    let response: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
    assert_eq!(response.error.unwrap().code, ERR_INVALID_PARAMS);

    drop(client_t);
    let _ = server_handle.await;
}

#[tokio::test]
async fn primitive_params_are_invalid_before_notification_suppression() {
    let (client_t, server_t) = in_memory_pair();
    let server = McpServer::new("cos", "0", registry_with_echo());
    let server_handle = tokio::spawn(server.serve(server_t));

    for (frame, expected_id) in [
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":true}"#,
            serde_json::json!(1),
        ),
        (
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":"bad"}"#,
            serde_json::json!(2),
        ),
        (
            r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":7}"#,
            serde_json::Value::Null,
        ),
    ] {
        client_t.send(frame.into()).await.unwrap();
        let frame = client_t.recv().await.unwrap().unwrap();
        let response: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(response["id"], expected_id);
        assert_eq!(response["error"]["code"], ERR_INVALID_REQUEST);
    }

    drop(client_t);
    let _ = server_handle.await;
}

#[tokio::test]
async fn fractional_and_large_numeric_ids_round_trip() {
    let (client_t, server_t) = in_memory_pair();
    let server = McpServer::new("cos", "0", registry_with_echo());
    let server_handle = tokio::spawn(server.serve(server_t));

    for request in [
        r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":9223372036854775808,"method":"ping"}"#,
    ] {
        client_t.send(request.into()).await.unwrap();
        let response = client_t.recv().await.unwrap().unwrap();
        let request_value: serde_json::Value = serde_json::from_str(request).unwrap();
        let response_value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response_value["id"], request_value["id"]);
    }

    drop(client_t);
    let _ = server_handle.await;
}
