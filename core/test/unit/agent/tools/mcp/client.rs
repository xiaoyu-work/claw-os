use super::super::transport::in_memory_pair;
use super::*;

impl McpClient {
    fn new_with_timeout(transport: impl Transport, timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            transport: Arc::new(transport),
            next_id: AtomicI64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            reader: Mutex::new(None),
            shutdown_tx: Mutex::new(None),
            request_timeout: timeout,
        })
    }
}

#[tokio::test]
async fn request_round_trip_via_in_memory_pair() {
    let (client_t, server_t) = in_memory_pair();
    let client = McpClient::new(client_t);
    client.start().await;

    // Spawn a tiny "server" that echoes every request back as
    // result = params.
    let server = tokio::spawn(async move {
        while let Ok(Some(frame)) = server_t.recv().await {
            let req: JsonRpcRequest = serde_json::from_str(&frame).unwrap();
            let resp = JsonRpcResponse::ok(req.id, req.params.unwrap_or(Value::Null));
            server_t
                .send(serde_json::to_string(&resp).unwrap())
                .await
                .unwrap();
        }
    });

    let result = client
        .request("ping", Some(serde_json::json!({"hello": "world"})))
        .await
        .unwrap();
    assert_eq!(result["hello"], "world");

    drop(client);
    let _ = server.await;
}

#[tokio::test]
async fn call_tool_context_is_injected_outside_arguments() {
    let (client_t, server_t) = in_memory_pair();
    let client = McpClient::new(client_t);
    client.start().await;

    let server = tokio::spawn(async move {
        let frame = server_t.recv().await.unwrap().unwrap();
        let request: JsonRpcRequest = serde_json::from_str(&frame).unwrap();
        let params = request.params.as_ref().unwrap();
        assert_eq!(params["name"], "email.search");
        assert_eq!(params["arguments"], serde_json::json!({"query": "customer"}));
        assert!(params["arguments"].get("caller").is_none());
        assert_eq!(
            params["_meta"]["claw-os.dev/call-context"]["caller"]["kind"],
            "system-agent"
        );
        let response = JsonRpcResponse::ok(
            request.id,
            serde_json::json!({"content": [], "isError": false}),
        );
        server_t
            .send(serde_json::to_string(&response).unwrap())
            .await
            .unwrap();
    });

    let context = crate::agent::tools::app_gateway::McpCallContext {
        wire_version: 1,
        call_id: "call-1".to_string(),
        trace_id: "trace-1".to_string(),
        parent_call_id: None,
        depth: 0,
        deadline_unix_ms: Some(1),
        session_id: Some("session-1".to_string()),
        task_id: Some("task-1".to_string()),
        caller: crate::agent::tools::app_gateway::McpPrincipal::system_agent(
            1000,
            "session-1",
        ),
    };
    client
        .call_tool_with_context(
            "email.search",
            Some(serde_json::json!({"query": "customer"})),
            context,
        )
        .await
        .unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn server_error_response_surfaces_as_client_error() {
    let (client_t, server_t) = in_memory_pair();
    let client = McpClient::new(client_t);
    client.start().await;

    let server = tokio::spawn(async move {
        let frame = server_t.recv().await.unwrap().unwrap();
        let req: JsonRpcRequest = serde_json::from_str(&frame).unwrap();
        let resp = JsonRpcResponse::err(
            req.id,
            JsonRpcError::new(super::super::protocol::ERR_METHOD_NOT_FOUND, "no method"),
        );
        server_t
            .send(serde_json::to_string(&resp).unwrap())
            .await
            .unwrap();
    });

    let err = client.request("missing", None).await.unwrap_err();
    match err {
        ClientError::Server { code, .. } => {
            assert_eq!(code, super::super::protocol::ERR_METHOD_NOT_FOUND)
        }
        other => panic!("expected Server error, got {other:?}"),
    }
    let _ = server.await;
}

#[tokio::test]
async fn closed_transport_yields_connection_closed() {
    let (client_t, server_t) = in_memory_pair();
    let client = McpClient::new(client_t);
    client.start().await;
    // Drop server side immediately.
    drop(server_t);
    // Give reader a tick to notice EOF.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let err = client.request("x", None).await.unwrap_err();
    // Either surfaces fine: send fails (Transport(Closed)) once
    // the peer's receiver is dropped, or the reader noticed EOF
    // first and we get ConnectionClosed when the oneshot is
    // dropped.
    assert!(
        matches!(
            err,
            ClientError::ConnectionClosed | ClientError::Transport(_)
        ),
        "expected ConnectionClosed or Transport, got {err:?}"
    );
}

/// A server that accepts the frame but never replies must result
/// in `ClientError::Timeout` rather than the client hanging
/// forever. Regression test for the missing per-request timeout.
#[tokio::test]
async fn client_per_request_timeout_fires_and_reaps_pending() {
    let (client_t, server_t) = in_memory_pair();
    // Use a very short timeout so the test is fast; production
    // path uses DEFAULT_REQUEST_TIMEOUT (60s).
    let client = McpClient::new_with_timeout(client_t, Duration::from_millis(50));
    client.start().await;

    // Server consumes one frame, then sleeps for a long time —
    // effectively "never responds" within the test window.
    let server = tokio::spawn(async move {
        let _ = server_t.recv().await;
        // Hold the transport alive so the reader doesn't hit EOF
        // (which would give us ConnectionClosed instead of the
        // Timeout we want to test).
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(server_t);
    });

    let started = std::time::Instant::now();
    let err = client.request("slow", None).await.unwrap_err();
    let elapsed = started.elapsed();
    match err {
        ClientError::Timeout(d) => {
            assert_eq!(d, Duration::from_millis(50));
        }
        other => panic!("expected Timeout, got {other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(2),
        "timeout must fire promptly, elapsed = {elapsed:?}"
    );

    // After a timeout the pending map must be empty — otherwise a
    // late response from the server would accumulate forever and
    // we'd leak a oneshot per timed-out request.
    let pending_len = client.pending.lock().await.len();
    assert_eq!(
        pending_len, 0,
        "timed-out request must be reaped from pending"
    );

    drop(client);
    let _ = server.await;
}
