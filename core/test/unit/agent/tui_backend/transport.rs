use super::super::backend::Operation;
use super::super::tests::{job, MockBackend, SESSION, TASK};
use super::super::Options;
use super::*;
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::WebSocketStream;

type Client = WebSocketStream<UnixStream>;

#[cfg(target_os = "linux")]
fn idle_listener() -> UnixListener {
    use std::os::linux::net::SocketAddrExt;

    let name = format!("claw-tui-shutdown-{}", uuid::Uuid::new_v4());
    let address = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind_addr(&address).unwrap();
    listener.set_nonblocking(true).unwrap();
    UnixListener::from_std(listener).unwrap()
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn listener_shutdown_is_prompt_before_any_client_connects() {
    let backend = MockBackend::new();
    let service = Arc::new(Service::new(backend.clone(), Options::default()));
    let (sender, receiver) = watch::channel(false);
    let server = tokio::spawn(serve(idle_listener(), service, 1000, receiver));
    tokio::task::yield_now().await;
    sender.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(backend.state.lock().unwrap().calls.is_empty());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn public_serve_honors_an_existing_shutdown_without_session_or_provider_queries() {
    if unsafe { libc::geteuid() } == 0 || unsafe { libc::getuid() } == 0 {
        return;
    }
    let (_sender, receiver) = watch::channel(true);
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        super::super::serve(
            idle_listener(),
            Arc::new(crate::config::CosConfig::default()),
            Options {
                session_id: Some("not-a-session".into()),
                ..Options::default()
            },
            receiver,
        ),
    )
    .await
    .unwrap();
    result.unwrap();
}

async fn connect(
    backend: &Arc<MockBackend>,
) -> (
    Client,
    watch::Sender<bool>,
    tokio::task::JoinHandle<Result<(), String>>,
) {
    let (server, client) = UnixStream::pair().unwrap();
    let (shutdown, receiver) = watch::channel(false);
    let connection = backend.connection(Options::default());
    let task = tokio::spawn(run_connection(server, connection, receiver));
    let (client, _) = tokio_tungstenite::client_async("ws://localhost/rpc", client)
        .await
        .unwrap();
    (client, shutdown, task)
}

async fn send_json(client: &mut Client, value: Value) {
    client
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

async fn receive(client: &mut Client) -> Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match client.next().await.unwrap().unwrap() {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Ping(_) | Message::Pong(_) => {}
                frame => panic!("unexpected WebSocket frame: {frame:?}"),
            }
        }
    })
    .await
    .expect("timed out waiting for a protocol message")
}

async fn reply(client: &mut Client, id: i64) -> Value {
    for _ in 0..100 {
        let value = receive(client).await;
        if value["id"] == id {
            return value;
        }
    }
    panic!("missing reply {id}")
}

async fn initialize(client: &mut Client) {
    send_json(
        client,
        json!({
            "id": 0,
            "method": "initialize",
            "params": {
                "clientInfo": { "name": "codex-tui", "version": "test" },
                "capabilities": { "experimentalApi": true },
            },
        }),
    )
    .await;
    let response = receive(client).await;
    assert_eq!(response["id"], 0);
    assert_eq!(response["result"]["platformFamily"], "unix");
    assert_eq!(response["result"]["codexHome"], "/home/claw/.config/cos");
    send_json(client, json!({ "method": "initialized" })).await;
}

#[test]
fn peer_policy_refuses_root_and_other_users() {
    assert!(peer_allowed(1000, 1000));
    assert!(!peer_allowed(1000, 1001));
    assert!(!peer_allowed(0, 0));
    assert!(!peer_allowed(1000, 0));
    assert!(process_peer_allowed(std::process::id() as i32));
    assert!(!process_peer_allowed(-1));
    assert!(!process_peer_allowed(unsafe { libc::getppid() }));
}

#[tokio::test]
async fn websocket_uses_the_real_upstream_initialize_sequence() {
    let backend = MockBackend::new();
    let (mut client, shutdown, task) = connect(&backend).await;
    send_json(
        &mut client,
        json!({ "id": 8, "method": "model/list", "params": {} }),
    )
    .await;
    assert_eq!(receive(&mut client).await["error"]["code"], -32001);
    initialize(&mut client).await;
    send_json(
        &mut client,
        json!({ "id": 9, "method": "model/list", "params": {} }),
    )
    .await;
    assert_eq!(
        reply(&mut client, 9).await["result"]["data"][0]["model"],
        "model-for-tests"
    );
    send_json(
        &mut client,
        json!({
            "id": 10, "method": "initialize",
            "params": { "clientInfo": { "name": "codex-tui", "version": "test" } },
        }),
    )
    .await;
    assert_eq!(reply(&mut client, 10).await["error"]["code"], -32600);
    shutdown.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn replies_can_arrive_out_of_order_without_losing_request_ids() {
    let backend = MockBackend::new();
    backend.state.lock().unwrap().hold = Some(Operation::ConversationGet);
    let (mut client, shutdown, task) = connect(&backend).await;
    initialize(&mut client).await;
    send_json(
        &mut client,
        json!({
            "id": 101, "method": "thread/read", "params": { "threadId": SESSION },
        }),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(1), backend.blocked.notified())
        .await
        .unwrap();
    send_json(
        &mut client,
        json!({ "id": 202, "method": "model/list", "params": {} }),
    )
    .await;
    assert_eq!(receive(&mut client).await["id"], 202);
    backend.release.notify_one();
    assert_eq!(receive(&mut client).await["id"], 101);
    shutdown.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn cancellation_does_not_wait_for_a_stream_poll_or_complete_a_running_task_early() {
    let backend = MockBackend::new();
    backend.seed_job(job("running"), Vec::new());
    backend.state.lock().unwrap().hold = Some(Operation::TaskStream);
    let (mut client, shutdown, task) = connect(&backend).await;
    initialize(&mut client).await;
    send_json(&mut client, json!({
        "id": 1, "method": "thread/resume", "params": { "threadId": SESSION, "excludeTurns": true },
    })).await;
    assert!(reply(&mut client, 1).await.get("result").is_some());
    tokio::time::timeout(Duration::from_secs(1), backend.blocked.notified())
        .await
        .unwrap();
    send_json(
        &mut client,
        json!({
            "id": 2, "method": "turn/interrupt", "params": { "threadId": SESSION, "turnId": TASK },
        }),
    )
    .await;
    loop {
        let message = receive(&mut client).await;
        assert_ne!(message["method"], "turn/completed");
        if message["id"] == 2 {
            assert_eq!(message["result"], json!({}));
            break;
        }
    }
    assert_eq!(backend.count(Operation::TaskCancel), 1);
    backend.state.lock().unwrap().jobs[0] = job("cancelled");
    backend.release.notify_one();
    loop {
        let message = receive(&mut client).await;
        if message["method"] == "turn/completed" {
            assert_eq!(message["params"]["turn"]["status"], "interrupted");
            assert_eq!(message["params"]["turn"]["id"], TASK);
            break;
        }
    }
    shutdown.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn shutdown_drops_subscriptions_without_cancelling_durable_tasks() {
    let backend = MockBackend::new();
    backend.seed_job(job("running"), Vec::new());
    backend.state.lock().unwrap().hold = Some(Operation::TaskStream);
    let (mut client, shutdown, task) = connect(&backend).await;
    initialize(&mut client).await;
    send_json(&mut client, json!({
        "id": 1, "method": "thread/resume", "params": { "threadId": SESSION, "excludeTurns": true },
    })).await;
    reply(&mut client, 1).await;
    tokio::time::timeout(Duration::from_secs(1), backend.blocked.notified())
        .await
        .unwrap();
    shutdown.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(backend.count(Operation::TaskCancel), 0);
}

#[tokio::test]
async fn rejects_oversized_websocket_frames_and_unfinished_http_headers() {
    let backend = MockBackend::new();
    let (mut client, _shutdown, task) = connect(&backend).await;
    let _ = client
        .send(Message::Text("x".repeat(MAX_MESSAGE_BYTES + 1).into()))
        .await;
    assert!(tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .is_err());

    let (server, mut client) = UnixStream::pair().unwrap();
    let (_shutdown, receiver) = watch::channel(false);
    let connection = backend.connection(Options::default());
    let task = tokio::spawn(run_connection(server, connection, receiver));
    let header = format!(
        "GET / HTTP/1.1\r\nX-Unfinished: {}",
        "x".repeat(HANDSHAKE_BYTES + 1024)
    );
    let _ = client.write_all(header.as_bytes()).await;
    assert!(tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .is_err());
}

#[tokio::test(start_paused = true)]
async fn outgoing_backpressure_is_bounded_and_fails_closed() {
    let backend = MockBackend::new();
    let connection = backend.connection(Options::default());
    let (sender, _receiver) = mpsc::channel(1);
    connection
        .emit(&sender, json!({ "id": 1, "result": {} }))
        .await
        .unwrap();
    let error = connection
        .emit(&sender, json!({ "id": 2, "result": {} }))
        .await
        .unwrap_err();
    assert_eq!(error.code, -32005);
    assert_eq!(sender.capacity(), 0);
}
