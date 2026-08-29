use super::*;
use crate::agent::tools::mcp::protocol::{
    CallToolResult, ContentItem, ListToolsResult, ToolDescriptor,
};
use crate::agent::tools::mcp::transport::{in_memory_pair, Transport};
use crate::agent::tools::registry::ToolRegistry;
use serde_json::json;

fn make_spec(name: &str) -> McpServerSpec {
    McpServerSpec {
        name: name.to_string(),
        command: "true".to_string(),
        args: Vec::new(),
        env: HashMap::new(),
        cwd: None,
        timeout_secs: 5,
        url: None,
        bearer_env: None,
    }
}

#[test]
fn timeout_duration_zero_means_unbounded() {
    let mut spec = make_spec("s");
    spec.timeout_secs = 0;
    assert_eq!(spec.timeout_duration(), Duration::from_secs(u64::MAX));
}

#[test]
fn timeout_duration_nonzero_is_passthrough() {
    let mut spec = make_spec("s");
    spec.timeout_secs = 17;
    assert_eq!(spec.timeout_duration(), Duration::from_secs(17));
}

#[test]
fn render_call_result_concatenates_text() {
    let res = CallToolResult {
        content: vec![
            ContentItem::Text {
                text: "hello".into(),
            },
            ContentItem::Text {
                text: "world".into(),
            },
        ],
        is_error: None,
    };
    let r = render_call_result("mcp_x_y", res);
    assert!(!r.is_error);
    // MCP results are wrapped in an untrusted-data boundary
    // (prompt-injection defense); the concatenated body lives inside.
    assert!(r.content.contains("hello\n\nworld"), "content: {}", r.content);
    assert!(
        r.content.contains("<untrusted_tool_result>"),
        "content: {}",
        r.content
    );
}

#[test]
fn render_call_result_marks_error_when_is_error_true() {
    let res = CallToolResult {
        content: vec![ContentItem::Text {
            text: "boom".into(),
        }],
        is_error: Some(true),
    };
    let r = render_call_result("mcp_x_y", res);
    assert!(r.is_error);
    assert!(r.content.contains("boom"), "content: {}", r.content);
    assert!(
        r.content.contains("<untrusted_tool_result>"),
        "content: {}",
        r.content
    );
}

#[test]
fn render_call_result_handles_empty_content() {
    let res = CallToolResult {
        content: Vec::new(),
        is_error: None,
    };
    let r = render_call_result("mcp_x_y", res);
    assert!(!r.is_error);
    assert!(r.content.contains("returned no content"));
}

#[test]
fn render_call_result_image_placeholder_mentions_mime() {
    let res = CallToolResult {
        content: vec![ContentItem::Image {
            data: "QUJD".into(),
            mime_type: "image/png".into(),
        }],
        is_error: None,
    };
    let r = render_call_result("mcp_x_y", res);
    assert!(r.content.contains("image/png"));
    assert!(r.content.contains("omitted"));
}

#[test]
fn mcp_remote_tool_uses_prefix_and_remote_name_round_trip() {
    let (client_t, _server_t) = in_memory_pair();
    let client = McpClient::new(client_t);
    let descriptor = ToolDescriptor {
        name: "query".to_string(),
        description: Some("run a query".to_string()),
        input_schema: json!({"type": "object", "properties": {"sql": {"type": "string"}}}),
    };
    let tool = McpRemoteTool::new("postgres", descriptor, client, Duration::from_secs(5));
    assert_eq!(tool.name(), "mcp_postgres_query");
    assert_eq!(tool.description(), "run a query");
    assert_eq!(tool.remote_name, "query");
    let schema = tool.input_schema();
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["sql"].is_object());
}

#[test]
fn mcp_remote_tool_falls_back_for_missing_description() {
    let (client_t, _server_t) = in_memory_pair();
    let client = McpClient::new(client_t);
    let descriptor = ToolDescriptor {
        name: "ping".to_string(),
        description: None,
        input_schema: json!({"type": "object"}),
    };
    let tool = McpRemoteTool::new("svc", descriptor, client, Duration::from_secs(5));
    assert!(tool.description().contains("ping"));
    assert!(tool.description().contains("svc"));
}

#[test]
fn mcp_remote_tool_coerces_non_object_schema() {
    let (client_t, _server_t) = in_memory_pair();
    let client = McpClient::new(client_t);
    let descriptor = ToolDescriptor {
        name: "no_args".to_string(),
        description: Some("trigger".into()),
        input_schema: Value::Null,
    };
    let tool = McpRemoteTool::new("svc", descriptor, client, Duration::from_secs(5));
    let schema = tool.input_schema();
    assert_eq!(schema["type"], "object");
    // additionalProperties on permissive fallback
    assert_eq!(schema["additionalProperties"], true);
}

#[tokio::test]
async fn routed_worker_never_falls_back_to_local_mcp_execution() {
    let spec = make_spec("isolated");
    let mut registry = ToolRegistry::new();
    let result = crate::paths::with_routed_job(attach_server(&spec, &mut registry)).await;
    let error = match result {
        Ok(_) => panic!("a worker without its host must fail closed"),
        Err(error) => error,
    };
    assert!(error.contains("extension host is unavailable"), "{error}");
}

/// End-to-end: a fake "MCP server" running in the same task pair
/// answers `tools/list` with one descriptor and `tools/call` with
/// a text payload. Verifies attach_server-equivalent flow against
/// the in-memory transport (we can't spawn a real subprocess in
/// unit tests portably).
#[tokio::test]
async fn end_to_end_in_memory_attach_flow_routes_call_through_prefixed_tool() {
    use crate::agent::tools::mcp::protocol::{
        InitializeResult, JsonRpcRequest, JsonRpcResponse, ServerCapabilities,
    };
    let (client_t, server_t) = in_memory_pair();
    let client = McpClient::new(client_t);
    client.start().await;

    let server_task = tokio::spawn(async move {
        for _ in 0..3 {
            let frame = match server_t.recv().await {
                Ok(Some(f)) => f,
                _ => break,
            };
            let req: JsonRpcRequest = serde_json::from_str(&frame).unwrap();
            let result = match req.method.as_str() {
                "initialize" => serde_json::to_value(InitializeResult {
                    protocol_version: PROTOCOL_VERSION.to_string(),
                    capabilities: ServerCapabilities::default(),
                    server_info: Implementation {
                        name: "fake".into(),
                        version: "0.0.1".into(),
                    },
                    instructions: None,
                })
                .unwrap(),
                "tools/list" => serde_json::to_value(ListToolsResult {
                    tools: vec![ToolDescriptor {
                        name: "say".into(),
                        description: Some("echo back".into()),
                        input_schema: json!({"type": "object"}),
                    }],
                    next_cursor: None,
                })
                .unwrap(),
                "tools/call" => serde_json::to_value(CallToolResult {
                    content: vec![ContentItem::Text {
                        text: "pong".into(),
                    }],
                    is_error: None,
                })
                .unwrap(),
                _ => json!({}),
            };
            let resp = JsonRpcResponse::ok(req.id, result);
            server_t
                .send(serde_json::to_string(&resp).unwrap())
                .await
                .unwrap();
        }
    });

    // Drive the same handshake `attach_server` performs, but
    // against the in-memory pair so we can avoid spawning.
    let init = client
        .initialize(
            Implementation {
                name: "test".into(),
                version: "0.0.0".into(),
            },
            ClientCapabilities::default(),
        )
        .await
        .unwrap();
    assert_eq!(init.server_info.name, "fake");
    let list = client.list_tools().await.unwrap();
    assert_eq!(list.tools.len(), 1);

    let mut registry = ToolRegistry::new();
    let descriptor = list.tools.into_iter().next().unwrap();
    let tool = McpRemoteTool::new("svc", descriptor, client.clone(), Duration::from_secs(5));
    assert_eq!(tool.name(), "mcp_svc_say");
    registry.register(Arc::new(tool));

    // Pull it back out of the registry and call it as the agent
    // loop would — `get` honours guardrails (we set none, so
    // permissive default permits everything).
    let dyn_tool = registry.get("mcp_svc_say").expect("tool registered");
    let result = dyn_tool.exec(json!({})).await;
    assert!(!result.is_error, "tool call should succeed: {:?}", result);
    // The remote result is wrapped in the untrusted-tool-result
    // boundary before it reaches the agent loop.
    assert!(result.content.contains("pong"), "content: {}", result.content);
    assert!(
        result.content.contains("<untrusted_tool_result>"),
        "content: {}",
        result.content
    );

    drop(client);
    let _ = server_task.await;
}
