use super::*;

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};
use tempfile::NamedTempFile;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::sync::Notify;

use crate::mcp::protocol::{CallToolResult, ContentItem};
use crate::mcp::transport::in_memory_pair;
use crate::mcp::MAX_MANIFEST_BYTES;

fn manifest(tools: Value) -> Value {
    json!({
        "schema_version": 2,
        "id": "test_app",
        "version": "1.2.3",
        "name": {"en": "Test App"},
        "mcp": {"transport": "stdio", "tools": tools}
    })
}

fn load_app(value: &Value) -> App {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(serde_json::to_string(value).unwrap().as_bytes())
        .unwrap();
    App::from_manifest(file.path()).unwrap()
}

fn protocol_only_app() -> App {
    let mut app = load_app(&manifest(json!([
        {
            "name": "echo",
            "summary": {"en": "Echo text"},
            "args": [{"name": "text", "kind": "text", "required": true}]
        }
    ])));
    app.bind(Arc::new(Echo)).unwrap();
    app
}

fn context(call_id: &str) -> Value {
    json!({
        "wire_version": 1,
        "call_id": call_id,
        "trace_id": format!("trace-{call_id}"),
        "depth": 1,
        "session_id": "session-1",
        "task_id": "task-1",
        "caller": {
            "kind": "app",
            "id": "caller",
            "owner_uid": 1000,
            "app_id": "caller_app"
        }
    })
}

fn call(id: Value, name: &str, arguments: Value, call_context: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments,
            "_meta": {CALL_CONTEXT_META_KEY: call_context}
        }
    })
}

async fn send(client: &impl Transport, value: Value) {
    client
        .send(serde_json::to_string(&value).unwrap())
        .await
        .unwrap();
}

async fn receive(client: &impl Transport) -> Value {
    let Frame::Message(frame) = client.recv().await.unwrap().unwrap() else {
        panic!("expected message frame");
    };
    serde_json::from_str(&frame).unwrap()
}

async fn receive_line(reader: &mut BufReader<DuplexStream>) -> Value {
    let mut line = String::new();
    let read = reader.read_line(&mut line).await.unwrap();
    assert!(
        read > 0,
        "expected a JSON-RPC response before output closed"
    );
    serde_json::from_str(line.trim_end()).unwrap()
}

struct Echo;

#[async_trait]
impl Tool for Echo {
    fn name(&self) -> &str {
        "echo"
    }

    async fn handle(&self, args: Value, _: CallContext) -> ToolResult {
        ToolResult::text(args["text"].as_str().unwrap())
    }
}

#[tokio::test]
async fn manifest_drives_identity_and_tools_list_schema() {
    let value = manifest(json!([
        {
            "name": "echo",
            "summary": {"en": "Echo text"},
            "args": [
                {
                    "name": "text",
                    "kind": "text",
                    "required": true,
                    "label": {"en": "Text to echo"}
                }
            ]
        }
    ]));
    let mut app = load_app(&value);
    assert_eq!(app.id(), "test_app");
    assert_eq!(app.version(), "1.2.3");
    app.bind(Arc::new(Echo)).unwrap();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    send(
        &client,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "1"}
            }
        }),
    )
    .await;
    let initialized = receive(&client).await;
    assert_eq!(initialized["result"]["serverInfo"]["name"], "test_app");
    assert_eq!(initialized["result"]["serverInfo"]["version"], "1.2.3");

    send(
        &client,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let listed = receive(&client).await;
    assert_eq!(listed["result"]["tools"][0]["description"], "Echo text");
    assert_eq!(
        listed["result"]["tools"][0]["inputSchema"],
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Text to echo"
                }
            },
            "required": ["text"],
            "additionalProperties": false
        })
    );

    drop(client);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn binding_routes_validated_call() {
    let mut app = load_app(&manifest(json!([
        {
            "name": "echo",
            "summary": {"en": "Echo text"},
            "args": [{"name": "text", "kind": "text", "required": true}]
        }
    ])));
    app.bind(Arc::new(Echo)).unwrap();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    send(
        &client,
        call(json!(1), "echo", json!({"text": "hi"}), context("c1")),
    )
    .await;
    let response = receive(&client).await;
    assert_eq!(response["result"]["content"][0]["text"], "hi");
    assert_eq!(response["result"]["isError"], false);

    drop(client);
    task.await.unwrap().unwrap();
}

#[test]
fn binding_rejects_undeclared_duplicates_and_missing_handlers() {
    let mut app = load_app(&manifest(json!([
        {"name": "echo", "summary": {"en": "Echo"}, "args": []}
    ])));
    assert!(matches!(
        app.bind(Arc::new(NamedTool("missing"))),
        Err(AppError::UndeclaredTool(name)) if name == "missing"
    ));
    app.bind(Arc::new(Echo)).unwrap();
    assert!(matches!(
        app.bind(Arc::new(Echo)),
        Err(AppError::DuplicateBinding(name)) if name == "echo"
    ));

    let app = load_app(&manifest(json!([
        {"name": "echo", "summary": {"en": "Echo"}, "args": []}
    ])));
    let (_, server) = in_memory_pair();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime.block_on(app.serve(server)).unwrap_err();
    assert!(matches!(error, AppError::MissingBindings(names) if names == "echo"));
}

struct NamedTool(&'static str);

#[async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &str {
        self.0
    }

    async fn handle(&self, _: Value, _: CallContext) -> ToolResult {
        ToolResult::text("")
    }
}

#[tokio::test]
async fn authenticated_context_is_mandatory_and_not_derived_from_args() {
    struct Inspect;
    #[async_trait]
    impl Tool for Inspect {
        fn name(&self) -> &str {
            "inspect"
        }

        async fn handle(&self, args: Value, context: CallContext) -> ToolResult {
            let snapshot = context.authenticated();
            ToolResult::text(format!(
                "{}:{}:{}:{}",
                context.caller().id,
                snapshot.trace_id,
                context.depth(),
                args["caller"]
            ))
        }
    }

    let mut app = load_app(&manifest(json!([
        {
            "name": "inspect",
            "summary": {"en": "Inspect"},
            "args": [{"name": "caller", "kind": "text", "required": true}]
        }
    ])));
    app.bind(Arc::new(Inspect)).unwrap();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    send(
        &client,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "inspect", "arguments": {"caller": "spoof"}}
        }),
    )
    .await;
    let missing = receive(&client).await;
    assert_eq!(missing["error"]["code"], ERR_INVALID_PARAMS);

    send(
        &client,
        call(
            json!(2),
            "inspect",
            json!({"caller": "spoof"}),
            context("immutable"),
        ),
    )
    .await;
    let response = receive(&client).await;
    assert_eq!(
        response["result"]["content"][0]["text"],
        "caller:trace-immutable:1:\"spoof\""
    );

    drop(client);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn defaults_unknown_args_types_choices_and_conditions_are_enforced() {
    struct Render;
    #[async_trait]
    impl Tool for Render {
        fn name(&self) -> &str {
            "render"
        }

        async fn handle(&self, args: Value, _: CallContext) -> ToolResult {
            ToolResult::text(serde_json::to_string(&args).unwrap())
        }
    }

    let mut app = load_app(&manifest(json!([
        {
            "name": "render",
            "summary": {"en": "Render"},
            "args": [
                {
                    "name": "mode",
                    "kind": "name",
                    "required": true,
                    "choices": ["quick", "full"]
                },
                {"name": "count", "kind": "integer", "default": 1.0},
                {
                    "name": "detail",
                    "kind": "text",
                    "required_when": {
                        "kind": "arg-equals",
                        "arg": "mode",
                        "value": "full"
                    }
                }
            ]
        }
    ])));
    app.bind(Arc::new(Render)).unwrap();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    for (id, arguments, expected) in [
        (1, json!({"mode": "quick", "extra": 1}), "unknown argument"),
        (2, json!({"mode": "bad"}), "allowed values"),
        (
            3,
            json!({"mode": "quick", "count": 1.5}),
            "must be an integer",
        ),
        (
            4,
            json!({"mode": "quick", "detail": "no"}),
            "condition is false",
        ),
        (5, json!({"mode": "full"}), "missing required argument"),
    ] {
        send(
            &client,
            call(json!(id), "render", arguments, context(&format!("c{id}"))),
        )
        .await;
        let response = receive(&client).await;
        assert_eq!(response["result"]["isError"], true);
        assert!(response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(expected));
    }

    send(
        &client,
        call(
            json!(6),
            "render",
            json!({"mode": "full", "detail": "all"}),
            context("c6"),
        ),
    )
    .await;
    let response = receive(&client).await;
    let rendered = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(rendered.contains(r#""count":1.0"#));
    assert!(rendered.contains(r#""detail":"all""#));

    drop(client);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn large_numeric_ids_and_arguments_preserve_lexemes() {
    struct NumberEcho;
    #[async_trait]
    impl Tool for NumberEcho {
        fn name(&self) -> &str {
            "number.echo"
        }

        async fn handle(&self, args: Value, _: CallContext) -> ToolResult {
            ToolResult::text(serde_json::to_string(&args["value"]).unwrap())
        }
    }
    let manifest: Value = serde_json::from_str(
        r#"{
          "schema_version":2,
          "id":"test_app",
          "version":"1.0.0",
          "name":{"en":"Test"},
          "mcp":{"tools":[{
            "name":"number.echo",
            "summary":{"en":"Number"},
            "args":[{
              "name":"value",
              "kind":"integer",
              "required":true,
              "choices":[18446744073709551616]
            }]
          }]}
        }"#,
    )
    .unwrap();
    let mut app = load_app(&manifest);
    app.bind(Arc::new(NumberEcho)).unwrap();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    client
        .send(
            r#"{"jsonrpc":"2.0","id":0.123456789012345678901234567890,"method":"tools/call","params":{"name":"number.echo","arguments":{"value":18446744073709551616},"_meta":{"claw-os.dev/call-context":{"wire_version":1,"call_id":"large","trace_id":"trace-large","depth":0,"caller":{"kind":"cli","id":"cli","owner_uid":1000}}}}}"#
                .into(),
        )
        .await
        .unwrap();
    let response = receive(&client).await;
    assert_eq!(
        serde_json::to_string(&response["id"]).unwrap(),
        "0.123456789012345678901234567890"
    );
    assert_eq!(
        response["result"]["content"][0]["text"],
        "18446744073709551616"
    );

    drop(client);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn structured_results_include_rendered_text_and_object() {
    struct Structured;
    #[async_trait]
    impl Tool for Structured {
        fn name(&self) -> &str {
            "structured"
        }

        async fn handle(&self, _: Value, _: CallContext) -> ToolResult {
            ToolResult::structured_with_text(json!({"ok": true}), "rendered").unwrap()
        }
    }
    let mut app = load_app(&manifest(json!([
        {"name": "structured", "summary": {"en": "Structured"}, "args": []}
    ])));
    app.bind(Arc::new(Structured)).unwrap();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    send(
        &client,
        call(json!(1), "structured", json!({}), context("structured")),
    )
    .await;
    let response = receive(&client).await;
    let result: CallToolResult = serde_json::from_value(response["result"].clone()).unwrap();
    assert_eq!(result.structured_content.unwrap()["ok"], true);
    assert!(matches!(
        result.content.first(),
        Some(ContentItem::Text { text }) if text == "rendered"
    ));

    drop(client);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn progress_is_emitted_only_with_a_token() {
    struct ProgressTool;
    #[async_trait]
    impl Tool for ProgressTool {
        fn name(&self) -> &str {
            "progress"
        }

        async fn handle(&self, _: Value, context: CallContext) -> ToolResult {
            context
                .report_progress(1.0, Progress::default().total(2.0).message("half"))
                .await
                .unwrap();
            ToolResult::text(context.progress_requested().to_string())
        }
    }
    let mut app = load_app(&manifest(json!([
        {"name": "progress", "summary": {"en": "Progress"}, "args": []}
    ])));
    app.bind(Arc::new(ProgressTool)).unwrap();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    let mut request = call(json!(1), "progress", json!({}), context("progress"));
    request["params"]["_meta"]["progressToken"] = json!("token");
    send(&client, request).await;
    let notification = receive(&client).await;
    assert_eq!(notification["method"], "notifications/progress");
    assert_eq!(notification["params"]["progressToken"], "token");
    assert_eq!(notification["params"]["message"], "half");
    let result = receive(&client).await;
    assert_eq!(result["result"]["content"][0]["text"], "true");

    send(
        &client,
        call(json!(2), "progress", json!({}), context("no-progress")),
    )
    .await;
    let result = receive(&client).await;
    assert_eq!(result["id"], 2);
    assert_eq!(result["result"]["content"][0]["text"], "false");

    drop(client);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn cancellation_while_running_suppresses_response_and_ping_stays_live() {
    struct Wait {
        started: Arc<Notify>,
        cancelled: Arc<AtomicBool>,
        progress_failed: Arc<AtomicBool>,
    }
    #[async_trait]
    impl Tool for Wait {
        fn name(&self) -> &str {
            "wait"
        }

        async fn handle(&self, _: Value, context: CallContext) -> ToolResult {
            self.started.notify_one();
            context.cancelled().await;
            self.cancelled
                .store(context.check_cancelled().is_err(), Ordering::Release);
            self.progress_failed.store(
                context
                    .report_progress(1.0, Progress::default())
                    .await
                    .is_err(),
                Ordering::Release,
            );
            ToolResult::text("obsolete")
        }
    }
    let started = Arc::new(Notify::new());
    let cancelled = Arc::new(AtomicBool::new(false));
    let progress_failed = Arc::new(AtomicBool::new(false));
    let mut app = load_app(&manifest(json!([
        {"name": "wait", "summary": {"en": "Wait"}, "args": []}
    ])));
    app.bind(Arc::new(Wait {
        started: started.clone(),
        cancelled: cancelled.clone(),
        progress_failed: progress_failed.clone(),
    }))
    .unwrap();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    let mut request = call(json!(1), "wait", json!({}), context("wait"));
    request["params"]["_meta"]["progressToken"] = json!("cancelled-progress");
    send(&client, request).await;
    started.notified().await;
    send(
        &client,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 1}
        }),
    )
    .await;
    send(
        &client,
        json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
    )
    .await;
    let response = receive(&client).await;
    assert_eq!(response["id"], 2);
    assert!(cancelled.load(Ordering::Acquire));
    assert!(progress_failed.load(Ordering::Acquire));

    drop(client);
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_wakes_all_context_clones_without_lost_notifications() {
    struct NoopProgress;

    #[async_trait]
    impl ProgressSink for NoopProgress {
        async fn emit_progress(&self, _: Value, _: f64, _: Progress) -> Result<(), TransportError> {
            Ok(())
        }
    }

    for round in 0..100 {
        let cancellation = Arc::new(Cancellation::new());
        let authenticated: McpCallContext =
            serde_json::from_value(context(&format!("race-{round}"))).unwrap();
        let call_context = CallContext::new(
            authenticated,
            cancellation.clone(),
            None,
            Arc::new(NoopProgress),
        );
        let barrier = Arc::new(tokio::sync::Barrier::new(34));
        let mut waiters = Vec::new();
        for _ in 0..32 {
            let barrier = barrier.clone();
            let context = call_context.clone();
            waiters.push(tokio::spawn(async move {
                barrier.wait().await;
                context.cancelled().await;
                context.check_cancelled().unwrap_err()
            }));
        }
        let cancel_barrier = barrier.clone();
        let cancel = cancellation.clone();
        let canceller = tokio::spawn(async move {
            cancel_barrier.wait().await;
            cancel.cancel("cancelled in race test").await;
        });
        barrier.wait().await;
        tokio::time::timeout(Duration::from_secs(1), canceller)
            .await
            .expect("canceller stalled")
            .unwrap();
        for waiter in waiters {
            let error = tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .expect("a cloned context missed cancellation")
                .unwrap();
            assert_eq!(error.reason(), "cancelled in race test");
        }
    }
}

#[tokio::test]
async fn authenticated_deadline_is_enforced() {
    struct Deadline;
    #[async_trait]
    impl Tool for Deadline {
        fn name(&self) -> &str {
            "deadline"
        }

        async fn handle(&self, _: Value, context: CallContext) -> ToolResult {
            context.cancelled().await;
            ToolResult::text("late")
        }
    }
    let mut app = load_app(&manifest(json!([
        {"name": "deadline", "summary": {"en": "Deadline"}, "args": []}
    ])));
    app.bind(Arc::new(Deadline)).unwrap();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    let mut authenticated = context("deadline");
    authenticated["deadline_unix_ms"] = json!(
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 25
    );
    send(
        &client,
        call(json!(1), "deadline", json!({}), authenticated),
    )
    .await;
    let response = receive(&client).await;
    assert_eq!(response["result"]["isError"], true);
    assert!(response["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("exceeded its deadline"));

    drop(client);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn schema_maximum_deadline_is_preserved_without_platform_time_conversion() {
    const MAX_SCHEMA_DEADLINE: u64 = 9_007_199_254_740_991;

    struct InspectDeadline;
    #[async_trait]
    impl Tool for InspectDeadline {
        fn name(&self) -> &str {
            "inspect.deadline"
        }

        async fn handle(&self, _: Value, context: CallContext) -> ToolResult {
            ToolResult::text(context.deadline_unix_ms().unwrap().to_string())
        }
    }

    let mut app = load_app(&manifest(json!([
        {
            "name": "inspect.deadline",
            "summary": {"en": "Inspect deadline"},
            "args": []
        }
    ])));
    app.bind(Arc::new(InspectDeadline)).unwrap();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    let mut authenticated = context("maximum-deadline");
    authenticated["deadline_unix_ms"] = json!(MAX_SCHEMA_DEADLINE);
    send(
        &client,
        call(json!(1), "inspect.deadline", json!({}), authenticated),
    )
    .await;
    let response = receive(&client).await;
    assert_eq!(
        response["result"]["content"][0]["text"],
        MAX_SCHEMA_DEADLINE.to_string()
    );

    drop(client);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn already_expired_deadline_returns_a_tool_error() {
    struct Expired;
    #[async_trait]
    impl Tool for Expired {
        fn name(&self) -> &str {
            "expired"
        }

        async fn handle(&self, _: Value, context: CallContext) -> ToolResult {
            context.cancelled().await;
            ToolResult::text("late")
        }
    }

    let mut app = load_app(&manifest(json!([
        {"name": "expired", "summary": {"en": "Expired"}, "args": []}
    ])));
    app.bind(Arc::new(Expired)).unwrap();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let mut authenticated = context("expired");
    authenticated["deadline_unix_ms"] = json!(now.saturating_sub(1).max(1));
    send(&client, call(json!(1), "expired", json!({}), authenticated)).await;
    let response = receive(&client).await;
    assert_eq!(response["result"]["isError"], true);
    assert!(response["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("exceeded its deadline"));

    drop(client);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn ping_round_trips_while_handler_runs() {
    struct Block {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }
    #[async_trait]
    impl Tool for Block {
        fn name(&self) -> &str {
            "block"
        }

        async fn handle(&self, _: Value, _: CallContext) -> ToolResult {
            self.started.notify_one();
            self.release.notified().await;
            ToolResult::text("done")
        }
    }
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut app = load_app(&manifest(json!([
        {"name": "block", "summary": {"en": "Block"}, "args": []}
    ])));
    app.bind(Arc::new(Block {
        started: started.clone(),
        release: release.clone(),
    }))
    .unwrap();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    send(
        &client,
        call(json!(1), "block", json!({}), context("block")),
    )
    .await;
    started.notified().await;
    send(
        &client,
        json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
    )
    .await;
    assert_eq!(receive(&client).await["id"], 2);
    release.notify_waiters();
    assert_eq!(
        receive(&client).await["result"]["content"][0]["text"],
        "done"
    );

    drop(client);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn stdio_reader_preserves_partial_next_frame_when_handler_completes() {
    struct Block {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }
    #[async_trait]
    impl Tool for Block {
        fn name(&self) -> &str {
            "block"
        }

        async fn handle(&self, _: Value, _: CallContext) -> ToolResult {
            self.started.notify_one();
            self.release.notified().await;
            ToolResult::text("done")
        }
    }

    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut app = load_app(&manifest(json!([
        {"name": "block", "summary": {"en": "Block"}, "args": []}
    ])));
    app.bind(Arc::new(Block {
        started: started.clone(),
        release: release.clone(),
    }))
    .unwrap();

    let (mut client_input, server_input) = tokio::io::duplex(64);
    let (server_output, client_output) = tokio::io::duplex(4096);
    let transport = StdioTransport::from_pair(Box::new(server_input), Box::new(server_output));
    let task = tokio::spawn(app.serve(transport));
    let mut output = BufReader::new(client_output);

    let first = serde_json::to_vec(&call(
        json!(1),
        "block",
        json!({}),
        context("partial-frame-block"),
    ))
    .unwrap();
    client_input.write_all(&first).await.unwrap();
    client_input.write_all(b"\n").await.unwrap();
    started.notified().await;

    let second = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "ping",
        "params": {"padding": "x".repeat(128 * 1024)}
    }))
    .unwrap();
    let split = second.len() / 2;
    assert!(split > 64);
    client_input.write_all(&second[..split]).await.unwrap();

    release.notify_waiters();
    let completed = receive_line(&mut output).await;
    assert_eq!(completed["id"], 1);
    assert_eq!(completed["result"]["content"][0]["text"], "done");

    client_input.write_all(&second[split..]).await.unwrap();
    client_input.write_all(b"\n").await.unwrap();
    let ping = receive_line(&mut output).await;
    assert_eq!(ping["id"], 2);
    assert_eq!(ping["result"], json!({}));

    drop(client_input);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn eof_grace_allows_a_nearly_complete_handler_to_respond() {
    struct FinishSoon {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }
    #[async_trait]
    impl Tool for FinishSoon {
        fn name(&self) -> &str {
            "finish.soon"
        }

        async fn handle(&self, _: Value, _: CallContext) -> ToolResult {
            self.started.notify_one();
            self.release.notified().await;
            ToolResult::text("finished")
        }
    }

    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut app = load_app(&manifest(json!([
        {"name": "finish.soon", "summary": {"en": "Finish soon"}, "args": []}
    ])));
    app.bind(Arc::new(FinishSoon {
        started: started.clone(),
        release: release.clone(),
    }))
    .unwrap();

    let (mut client_input, server_input) = tokio::io::duplex(4096);
    let (server_output, client_output) = tokio::io::duplex(4096);
    let transport = StdioTransport::from_pair(Box::new(server_input), Box::new(server_output));
    let task = tokio::spawn(app.serve(transport));
    let mut output = BufReader::new(client_output);

    let request = serde_json::to_vec(&call(
        json!(1),
        "finish.soon",
        json!({}),
        context("eof-grace"),
    ))
    .unwrap();
    client_input.write_all(&request).await.unwrap();
    client_input.write_all(b"\n").await.unwrap();
    started.notified().await;
    drop(client_input);

    tokio::time::sleep(Duration::from_millis(10)).await;
    release.notify_waiters();
    let response = tokio::time::timeout(Duration::from_secs(1), receive_line(&mut output))
        .await
        .expect("EOF grace expired before a near-complete handler responded");
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["content"][0]["text"], "finished");
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("runtime did not exit after EOF grace work completed")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn handler_panic_is_a_tool_error_and_app_survives() {
    struct Panic;
    #[async_trait]
    impl Tool for Panic {
        fn name(&self) -> &str {
            "panic"
        }

        async fn handle(&self, _: Value, _: CallContext) -> ToolResult {
            panic!("boom")
        }
    }
    let mut app = load_app(&manifest(json!([
        {"name": "panic", "summary": {"en": "Panic"}, "args": []},
        {
            "name": "echo",
            "summary": {"en": "Echo"},
            "args": [{"name": "text", "kind": "text", "required": true}]
        }
    ])));
    app.bind(Arc::new(Panic)).unwrap();
    app.bind(Arc::new(Echo)).unwrap();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    send(
        &client,
        call(json!(1), "panic", json!({}), context("panic")),
    )
    .await;
    let panic_response = receive(&client).await;
    assert_eq!(panic_response["result"]["isError"], true);
    assert!(panic_response["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("panicked"));

    send(
        &client,
        call(json!(2), "echo", json!({"text": "alive"}), context("alive")),
    )
    .await;
    assert_eq!(
        receive(&client).await["result"]["content"][0]["text"],
        "alive"
    );

    drop(client);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn malformed_and_oversized_custom_frames_recover() {
    let app = protocol_only_app();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    client.send("not json".into()).await.unwrap();
    assert_eq!(receive(&client).await["error"]["code"], ERR_PARSE);

    client.send("x".repeat(MAX_FRAME_BYTES + 1)).await.unwrap();
    assert_eq!(receive(&client).await["error"]["code"], ERR_PARSE);

    send(
        &client,
        json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}),
    )
    .await;
    assert_eq!(receive(&client).await["id"], 3);

    drop(client);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn invalid_utf8_stdio_frame_returns_parse_error_and_next_ping_succeeds() {
    let app = protocol_only_app();
    let (mut client_input, server_input) = tokio::io::duplex(4096);
    let (server_output, client_output) = tokio::io::duplex(4096);
    let transport = StdioTransport::from_pair(Box::new(server_input), Box::new(server_output));
    let task = tokio::spawn(app.serve(transport));
    let mut output = BufReader::new(client_output);

    client_input.write_all(b"{\xff}\n").await.unwrap();
    client_input
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n")
        .await
        .unwrap();

    let parse_error = receive_line(&mut output).await;
    assert_eq!(parse_error["error"]["code"], ERR_PARSE);
    assert_eq!(parse_error["id"], Value::Null);
    let ping = receive_line(&mut output).await;
    assert_eq!(ping["id"], 2);
    assert_eq!(ping["result"], json!({}));

    drop(client_input);
    task.await.unwrap().unwrap();
}

struct FailingOutput {
    frame: StdMutex<Option<Frame>>,
}

#[async_trait]
impl Transport for FailingOutput {
    async fn send(&self, _: String) -> Result<(), TransportError> {
        Err(TransportError::Io("broken output".into()))
    }

    async fn recv(&self) -> Result<Option<Frame>, TransportError> {
        Ok(self.frame.lock().unwrap().take())
    }
}

#[tokio::test]
async fn output_failure_is_returned() {
    let app = protocol_only_app();
    let error = app
        .serve(FailingOutput {
            frame: StdMutex::new(Some(Frame::Message(
                r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.into(),
            ))),
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AppError::Transport(TransportError::Io(message)) if message == "broken output"
    ));
}

struct PanickingInput;

#[async_trait]
impl Transport for PanickingInput {
    async fn send(&self, _: String) -> Result<(), TransportError> {
        Ok(())
    }

    async fn recv(&self) -> Result<Option<Frame>, TransportError> {
        panic!("reader failed")
    }
}

#[tokio::test]
async fn reader_task_failure_is_returned_instead_of_closing_silently() {
    let app = protocol_only_app();
    let error = tokio::time::timeout(Duration::from_secs(1), app.serve(PanickingInput))
        .await
        .expect("runtime hung after reader task failure")
        .unwrap_err();
    assert!(matches!(
        error,
        AppError::Transport(TransportError::Io(message))
            if message.contains("MCP reader task failed")
                && message.contains("reader failed")
    ));
}

#[tokio::test]
async fn at_most_sixty_four_active_and_pending_calls_are_accepted() {
    struct Never {
        started: Arc<Notify>,
    }
    #[async_trait]
    impl Tool for Never {
        fn name(&self) -> &str {
            "never"
        }

        async fn handle(&self, _: Value, context: CallContext) -> ToolResult {
            self.started.notify_one();
            context.cancelled().await;
            ToolResult::text("")
        }
    }
    let started = Arc::new(Notify::new());
    let mut app = load_app(&manifest(json!([
        {"name": "never", "summary": {"en": "Never"}, "args": []}
    ])));
    app.bind(Arc::new(Never {
        started: started.clone(),
    }))
    .unwrap();
    let (client, server) = in_memory_pair();
    let task = tokio::spawn(app.serve(server));

    for id in 1..=65 {
        send(
            &client,
            call(json!(id), "never", json!({}), context(&format!("c{id}"))),
        )
        .await;
    }
    started.notified().await;
    let busy = receive(&client).await;
    assert_eq!(busy["id"], 65);
    assert_eq!(busy["error"]["code"], ERR_SERVER_BUSY);

    drop(client);
    task.await.unwrap().unwrap();
}

#[test]
fn manifest_validation_rejects_session_only_fields_and_invalid_contracts() {
    for value in [
        json!({
            "id": "test_app",
            "version": "1.0.0",
            "name": {"en": "Test"},
            "mcp": {"tools": []}
        }),
        manifest(json!([
            {"name": "Bad", "summary": {"en": "Bad"}, "args": []}
        ])),
        manifest(json!([
            {"name": "dup", "summary": {"en": "One"}, "args": []},
            {"name": "dup", "summary": {"en": "Two"}, "args": []}
        ])),
        manifest(json!([
            {"name": "empty", "summary": {"en": "  "}, "args": []}
        ])),
        manifest(json!([
            {
                "name": "legacy",
                "summary": {"en": "Legacy"},
                "args": [{"name": "x", "kind": "text", "default_from": {"arg": "y"}}]
            }
        ])),
        manifest(json!([])),
        {
            let mut value = manifest(json!([
                {"name": "closed", "summary": {"en": "Closed"}}
            ]));
            value["unknown"] = json!(true);
            value
        },
        {
            let mut value = manifest(json!([
                {"name": "closed", "summary": {"en": "Closed"}}
            ]));
            value["mcp"]["unknown"] = json!(true);
            value
        },
        manifest(json!([
            {"name": "closed", "summary": {"en": "Closed"}, "unknown": true}
        ])),
        manifest(json!([
            {
                "name": "closed",
                "summary": {"en": "Closed"},
                "args": [{"name": "x", "kind": "text", "binding": "flag"}]
            }
        ])),
    ] {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(serde_json::to_string(&value).unwrap().as_bytes())
            .unwrap();
        assert!(matches!(
            App::from_manifest(file.path()),
            Err(AppError::Manifest(_))
        ));
    }
}

#[test]
fn every_required_when_kind_is_applied() {
    let app = load_app(&manifest(json!([
        {
            "name": "conditions",
            "summary": {"en": "Conditions"},
            "args": [
                {"name": "flag", "kind": "bool"},
                {"name": "mode", "kind": "name", "required": true},
                {
                    "name": "when_present",
                    "kind": "text",
                    "required_when": {"kind": "arg-present", "arg": "flag"}
                },
                {
                    "name": "when_full",
                    "kind": "text",
                    "required_when": {
                        "kind": "arg-equals",
                        "arg": "mode",
                        "value": "full"
                    }
                },
                {
                    "name": "when_not_quick",
                    "kind": "text",
                    "required_when": {
                        "kind": "arg-not-equals",
                        "arg": "mode",
                        "value": "quick"
                    }
                }
            ]
        }
    ])));
    let tool = &app.tools[0];
    assert_eq!(tool.input_schema["allOf"][0]["if"]["required"][0], "flag");
    assert_eq!(
        tool.input_schema["allOf"][1]["if"]["properties"]["mode"]["const"],
        "full"
    );
    assert_eq!(
        tool.input_schema["allOf"][2]["if"]["not"]["properties"]["mode"]["const"],
        "quick"
    );

    let quick = manifest::resolve_arguments(tool, json!({"mode": "quick"}).as_object().unwrap());
    assert!(quick.is_ok());
    let present = manifest::resolve_arguments(
        tool,
        json!({"flag": false, "mode": "quick"}).as_object().unwrap(),
    )
    .unwrap_err();
    assert!(present.contains("when_present"));
    let full = manifest::resolve_arguments(tool, json!({"mode": "full"}).as_object().unwrap())
        .unwrap_err();
    assert!(full.contains("when_full") || full.contains("when_not_quick"));
    let invalid_inactive = manifest::resolve_arguments(
        tool,
        json!({"mode": "quick", "when_not_quick": "no"})
            .as_object()
            .unwrap(),
    )
    .unwrap_err();
    assert!(invalid_inactive.contains("condition is false"));
}

#[test]
fn environment_loader_and_manifest_size_cap_are_enforced() {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(
        serde_json::to_string(&manifest(json!([
            {"name": "status", "summary": {"en": "Status"}}
        ])))
        .unwrap()
        .as_bytes(),
    )
    .unwrap();
    let prior = std::env::var_os("COS_APP_MANIFEST");
    std::env::set_var("COS_APP_MANIFEST", file.path());
    assert_eq!(App::from_environment().unwrap().id(), "test_app");
    match prior {
        Some(value) => std::env::set_var("COS_APP_MANIFEST", value),
        None => std::env::remove_var("COS_APP_MANIFEST"),
    }

    let mut oversized = NamedTempFile::new().unwrap();
    oversized
        .write_all(&vec![b' '; MAX_MANIFEST_BYTES + 1])
        .unwrap();
    let error = match App::from_manifest(oversized.path()) {
        Ok(_) => panic!("oversized manifest unexpectedly loaded"),
        Err(error) => error,
    };
    assert!(matches!(error, AppError::Manifest(message) if message.contains("exceeds")));
}
