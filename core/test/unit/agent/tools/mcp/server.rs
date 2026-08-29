use super::super::client::{ClientError, McpClient};
use super::super::protocol::{ClientCapabilities, ContentItem, Implementation};
use super::super::transport::in_memory_pair;
use super::*;
use crate::agent::tools::builtin::Echo;

struct Restricted;

struct ExtensionEcho;

#[async_trait::async_trait]
impl crate::agent::tools::Tool for ExtensionEcho {
    fn name(&self) -> &str {
        "mcp_alpha_echo"
    }

    fn description(&self) -> &str {
        "Echo through a deferred MCP tool."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
            "additionalProperties": false,
        })
    }

    fn disclosure(&self) -> crate::agent::tools::progressive::ToolDisclosure {
        crate::agent::tools::progressive::ToolDisclosure::extension(
            "mcp",
            Some("alpha".to_string()),
            Some("echo".to_string()),
            ["mcp".to_string()],
        )
    }

    async fn exec(&self, input: serde_json::Value) -> crate::agent::tools::ToolResult {
        crate::agent::tools::ToolResult::ok(input["text"].as_str().unwrap_or_default())
    }
}

#[async_trait::async_trait]
impl crate::agent::tools::Tool for Restricted {
    fn name(&self) -> &str {
        "restricted"
    }

    fn description(&self) -> &str {
        "requires fs.read"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn exposure(&self) -> crate::agent::tools::exposure::ToolExposure {
        crate::agent::tools::exposure::ToolExposure::always()
            .requiring_all_verbs([crate::caps::Verb::FS_READ])
    }

    async fn exec(&self, _input: serde_json::Value) -> crate::agent::tools::ToolResult {
        crate::agent::tools::ToolResult::ok("should not run")
    }
}

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
async fn large_catalog_is_listed_and_called_through_stable_bridge() {
    let (client_t, server_t) = in_memory_pair();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(Echo));
    registry.register(Arc::new(ExtensionEcho));
    let context = crate::agent::tools::exposure::ToolExposureContext::isolated(
        crate::agent::tools::guardrails::Guardrails::permissive(),
    )
    .with_tool_schema_budget_tokens(0);
    let server = McpServer::new_with_context("cos", "0", Arc::new(registry), context);
    let server_handle = tokio::spawn(server.serve(server_t));

    let client = McpClient::new(client_t);
    client.start().await;
    let listing = client.list_tools().await.unwrap();
    let names = listing
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"echo"));
    assert!(names.contains(&crate::agent::tools::progressive::TOOL_SEARCH));
    assert!(names.contains(&crate::agent::tools::progressive::TOOL_DESCRIBE));
    assert!(names.contains(&crate::agent::tools::progressive::TOOL_CALL));
    assert!(!names.contains(&"mcp_alpha_echo"));

    let result = client
        .call_tool(
            crate::agent::tools::progressive::TOOL_CALL,
            Some(serde_json::json!({
                "name": "mcp_alpha_echo",
                "arguments": {"text": "hello"}
            })),
        )
        .await
        .unwrap();
    assert_eq!(result.is_error, None);
    assert!(matches!(
        result.content.first(),
        Some(ContentItem::Text { text }) if text == "hello"
    ));

    let direct = client
        .call_tool(
            "mcp_alpha_echo",
            Some(serde_json::json!({"text": "bypass"})),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        direct,
        ClientError::Server {
            code: ERR_INVALID_PARAMS,
            ..
        }
    ));
    drop(client);
    let _ = server_handle.await;
}

#[tokio::test]
async fn unavailable_tools_are_neither_listed_nor_callable() {
    let (client_t, server_t) = in_memory_pair();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(Restricted));
    let context = crate::agent::tools::exposure::ToolExposureContext::isolated(
        crate::agent::tools::guardrails::Guardrails::permissive(),
    );
    let server = McpServer::new_with_context("cos", "0", Arc::new(registry), context);
    let server_handle = tokio::spawn(server.serve(server_t));

    let client = McpClient::new(client_t);
    client.start().await;
    assert!(client.list_tools().await.unwrap().tools.is_empty());
    let error = client
        .call_tool("restricted", Some(json!({})))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ClientError::Server {
            code: ERR_INVALID_PARAMS,
            ..
        }
    ));
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

    for (frame, expected_id, expected_code) in [
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":true}"#,
            serde_json::json!(1),
            ERR_INVALID_PARAMS,
        ),
        (
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":"bad"}"#,
            serde_json::json!(2),
            ERR_INVALID_PARAMS,
        ),
        (
            r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":7}"#,
            serde_json::Value::Null,
            ERR_INVALID_REQUEST,
        ),
    ] {
        client_t.send(frame.into()).await.unwrap();
        let frame = client_t.recv().await.unwrap().unwrap();
        let response: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(response["id"], expected_id);
        assert_eq!(response["error"]["code"], expected_code);
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
        r#"{"jsonrpc":"2.0","id":0.123456789012345678901234567890,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":18446744073709551616,"method":"ping"}"#,
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

#[tokio::test]
async fn initialize_rejects_null_or_malformed_known_capabilities() {
        let (client_t, server_t) = in_memory_pair();
        let server = McpServer::new("cos", "0", registry_with_echo());
        let server_handle = tokio::spawn(server.serve(server_t));

        for capabilities in [
            r#"{"roots":null}"#,
            r#"{"roots":false}"#,
            r#"{"roots":{"listChanged":null}}"#,
            r#"{"roots":{"listChanged":"yes"}}"#,
            r#"{"experimental":null}"#,
            r#"{"sampling":[]}"#,
            r#"{"elicitation":null}"#,
            r#"{"elicitation":false}"#,
        ] {
            let request = format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{capabilities},"clientInfo":{{"name":"test","version":"1"}}}}}}"#
            );
            client_t.send(request).await.unwrap();
            let frame = client_t.recv().await.unwrap().unwrap();
            let response: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
            assert_eq!(response.error.unwrap().code, ERR_INVALID_PARAMS);
        }

        drop(client_t);
        let _ = server_handle.await;
}

#[tokio::test]
async fn ping_and_tools_list_validate_method_specific_params() {
        let (client_t, server_t) = in_memory_pair();
        let server = McpServer::new("cos", "0", registry_with_echo());
        let server_handle = tokio::spawn(server.serve(server_t));

        for request in [
            r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":[]}"#,
            r#"{"jsonrpc":"2.0","id":7,"method":"ping","params":null}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":[]}"#,
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/list","params":null}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{"cursor":7}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/list","params":{"cursor":null}}"#,
        ] {
            client_t.send(request.into()).await.unwrap();
            let frame = client_t.recv().await.unwrap().unwrap();
            let response: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
            assert_eq!(response.error.unwrap().code, ERR_INVALID_PARAMS);
        }

        for request in [
            r#"{"jsonrpc":"2.0","id":5,"method":"ping","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/list","params":{"cursor":"next"}}"#,
        ] {
            client_t.send(request.into()).await.unwrap();
            let frame = client_t.recv().await.unwrap().unwrap();
            let response: JsonRpcResponse = serde_json::from_str(&frame).unwrap();
            assert!(response.error.is_none());
        }

        drop(client_t);
        let _ = server_handle.await;
}

#[tokio::test]
async fn invalid_id_precedes_null_method_params() {
        let (client_t, server_t) = in_memory_pair();
        let server = McpServer::new("cos", "0", registry_with_echo());
        let server_handle = tokio::spawn(server.serve(server_t));

        client_t
            .send(r#"{"jsonrpc":"2.0","id":true,"method":"ping","params":null}"#.into())
            .await
            .unwrap();
        let frame = client_t.recv().await.unwrap().unwrap();
        let response: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(response["id"], serde_json::Value::Null);
        assert_eq!(response["error"]["code"], ERR_INVALID_REQUEST);

        drop(client_t);
        let _ = server_handle.await;
}
