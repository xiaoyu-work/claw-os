use super::*;
use serde_json::json;
use tokio::net::UnixStream;

use crate::clawd::routes::{Access, Kind, ROUTES};
use crate::clawd::transport::frame::{encode_frame, read_response_async};
use crate::clawd::wire::{KIND_REQUEST, MAX_RESPONSE_BYTES};

fn peer(uid: u32) -> ClientIdentity {
    ClientIdentity {
        pid: Some(std::process::id()),
        uid: Some(uid),
        gid: Some(uid),
        execution_uid: None,
        start_time_ticks: Some(7),
        attended_local: false,
        extension_host: None,
    }
}

fn envelope(id: &str, command: &str, params: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "v": PROTOCOL_VERSION,
        "id": id,
        "command": command,
        "params": params,
    }))
    .unwrap()
}

fn admission() -> Arc<Admission> {
    Admission::new(Limits::default())
}

// ---------------------------------------------------------------------------
// Admission order
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_well_formed_request_is_admitted_with_its_typed_body() {
    let admission = admission();
    let body = envelope(
        "r-1",
        "memory.history",
        json!({
            "session_id": "sess-1",
            "limit": 5,
        }),
    );
    let admitted = admit(&body, &peer(1000), &admission)
        .await
        .unwrap_or_else(|error| {
            panic!("expected admission, got {:?}", error.fault);
        });
    assert_eq!(admitted.route.name, "memory.history");
    assert_eq!(admitted.id.as_str(), "r-1");
    assert_eq!(admitted.params, json!({"session_id": "sess-1", "limit": 5}));
    assert!(
        admitted.decision.is_none(),
        "a peer-scoped route resolves no capability grant"
    );
}

#[tokio::test]
async fn a_privileged_provider_request_without_a_grant_is_refused() {
    // Naming a session is not authority: with no grant behind the id,
    // the request never reaches the provider.
    let admission = admission();
    let body = envelope(
        "r-1",
        "system.package.control",
        json!({
            "session": "sess-1",
            "action": "remove",
            "package": "nano",
        }),
    );
    let refusal = admit(&body, &peer(1000), &admission)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.fault, Fault::NotAuthorized);
    assert_eq!(refusal.command, Some("system.package.control"));
}

#[tokio::test]
async fn an_envelope_from_another_protocol_version_fails_closed() {
    let admission = admission();
    let body = serde_json::to_vec(&json!({
        "v": PROTOCOL_VERSION + 1,
        "id": "r-1",
        "command": "daemon.health",
        "params": {},
    }))
    .unwrap();
    let refusal = admit(&body, &peer(1000), &admission)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.fault, Fault::UnsupportedVersion);
    assert_eq!(refusal.id.as_str(), "r-1", "the id is still echoed");
    assert!(refusal.command.is_none());
}

#[tokio::test]
async fn a_pre_v1_body_is_refused_rather_than_downgraded() {
    let admission = admission();
    // Exactly what an out-of-date client used to send: no version, an
    // untyped id, and a bare command string.
    let body = br#"{"id":1,"command":"daemon.health","params":{}}"#;
    let refusal = admit(body, &peer(1000), &admission)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.fault, Fault::InvalidEnvelope);
}

#[tokio::test]
async fn a_body_that_is_not_json_is_refused_as_malformed() {
    let admission = admission();
    let refusal = admit(b"\x00\x01\x02not json", &peer(1000), &admission)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.fault, Fault::MalformedBody);
    assert_eq!(refusal.id.as_str(), "unknown");
}

#[tokio::test]
async fn deeply_nested_json_is_refused_before_a_route_sees_it() {
    let admission = admission();
    let deep = format!(
        "{}{}{}",
        r#"{"v":2,"id":"r-1","command":"context.update","params":{"source":"s","payload":"#,
        "[".repeat(400),
        "]".repeat(400)
    ) + "}}";
    let refusal = admit(deep.as_bytes(), &peer(0), &admission)
        .await
        .err()
        .expect("refused");
    assert!(
        matches!(refusal.fault, Fault::MalformedBody | Fault::InvalidParams),
        "{:?}",
        refusal.fault
    );
}

#[tokio::test]
async fn a_structurally_deep_payload_inside_the_parser_limit_is_still_refused() {
    let admission = admission();
    let mut payload = json!(1);
    for _ in 0..40 {
        payload = Value::Array(vec![payload]);
    }
    let body = envelope(
        "r-1",
        "context.update",
        json!({"source": "s", "payload": payload}),
    );
    let refusal = admit(&body, &peer(0), &admission)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.fault, Fault::InvalidParams);
    assert_eq!(refusal.command, Some("context.update"));
}

#[tokio::test]
async fn an_unknown_command_fails_closed_before_authorization() {
    let admission = admission();
    let body = envelope("r-1", "vendor.debug.dump", json!({}));
    let refusal = admit(&body, &peer(1000), &admission)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.fault, Fault::UnknownCommand);
    assert_eq!(refusal.fault.code(), "unknown_command");
    assert!(
        refusal.command.is_none(),
        "the caller's own string must never become a recorded route name"
    );
}

#[tokio::test]
async fn an_undeclared_field_fails_closed_before_dispatch() {
    let admission = admission();
    let body = envelope(
        "r-1",
        "system.power.control",
        json!({"session": "s", "action": "off", "owner_uid": 0}),
    );
    let refusal = admit(&body, &peer(1000), &admission)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.fault, Fault::InvalidParams);
    assert_eq!(refusal.fault.code(), "invalid_request");
    assert_eq!(refusal.command, Some("system.power.control"));
}

#[tokio::test]
async fn a_user_peer_cannot_reach_a_root_route() {
    let admission = admission();
    let body = envelope("r-1", "context.update", json!({"source": "probe"}));
    let refusal = admit(&body, &peer(1000), &admission)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.fault, Fault::NotAuthorized);
    assert_eq!(refusal.fault.code(), "not_authorized");

    let admitted = admit(&body, &peer(0), &admission)
        .await
        .unwrap_or_else(|error| panic!("root must reach a root route, got {:?}", error.fault));
    assert_eq!(admitted.route.name, "context.update");
}

#[tokio::test]
async fn a_request_without_verified_credentials_is_refused() {
    let admission = admission();
    let body = envelope("r-1", "daemon.health", json!({}));
    let refusal = admit(&body, &ClientIdentity::unknown(), &admission)
        .await
        .err()
        .expect("refused");
    assert_eq!(refusal.fault, Fault::MissingCredentials);
}

#[tokio::test]
async fn a_replayed_mutation_is_refused_but_a_replayed_query_is_not() {
    let admission = admission();
    let mutation = envelope("r-dup", "task.cancel", json!({"id": "task-1"}));
    assert!(admit(&mutation, &peer(1000), &admission).await.is_ok());
    let refusal = admit(&mutation, &peer(1000), &admission)
        .await
        .err()
        .expect("a repeated mutation id must be refused");
    assert_eq!(refusal.fault, Fault::DuplicateRequest);

    let query = envelope("r-dup", "task.get", json!({"id": "task-1"}));
    assert!(admit(&query, &peer(1000), &admission).await.is_ok());
    assert!(
        admit(&query, &peer(1000), &admission).await.is_ok(),
        "an idempotent read may be repeated"
    );
}

#[tokio::test]
async fn a_route_flood_is_refused_once_its_budget_is_full() {
    let admission = admission();
    let route = Command::TransactionRollback.route();
    let mut held = Vec::new();
    for index in 0..route.budget.max_in_flight {
        let body = envelope(
            &format!("r-{index}"),
            "transaction.rollback",
            json!({"id": "tx-1"}),
        );
        held.push(
            admit(&body, &peer(1000), &admission)
                .await
                .unwrap_or_else(|error| panic!("within budget, got {:?}", error.fault)),
        );
    }
    let body = envelope("r-over", "transaction.rollback", json!({"id": "tx-1"}));
    assert_eq!(
        admit(&body, &peer(1000), &admission)
            .await
            .err()
            .map(|r| r.fault),
        Some(Fault::RouteBusy)
    );
    drop(held);
}

#[tokio::test]
async fn one_principal_cannot_hold_every_in_flight_slot() {
    let admission = Admission::new(Limits {
        max_in_flight_per_user: 2,
        ..Limits::default()
    });
    let mut held = Vec::new();
    for index in 0..2 {
        let body = envelope(&format!("r-{index}"), "task.list", json!({}));
        held.push(
            admit(&body, &peer(1000), &admission)
                .await
                .expect("within budget"),
        );
    }
    let body = envelope("r-over", "task.list", json!({}));
    assert_eq!(
        admit(&body, &peer(1000), &admission)
            .await
            .err()
            .map(|r| r.fault),
        Some(Fault::TooManyRequests)
    );
    // A different principal is unaffected.
    let other = envelope("r-other", "task.list", json!({}));
    assert!(admit(&other, &peer(1001), &admission).await.is_ok());
    drop(held);
}

// ---------------------------------------------------------------------------
// Whole-socket behaviour
// ---------------------------------------------------------------------------

struct DataDir {
    _dir: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl DataDir {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let previous = std::env::var_os("COS_DATA_DIR");
        std::env::set_var("COS_DATA_DIR", dir.path());
        Self {
            _dir: dir,
            previous,
        }
    }
}

impl Drop for DataDir {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var("COS_DATA_DIR", value),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
    }
}

async fn exchange(request: &[u8], limits: Limits) -> Value {
    let raw = exchange_raw(request, limits).await;
    serde_json::from_slice(&raw).expect("a v1 response frame")
}

async fn exchange_raw(request: &[u8], limits: Limits) -> Vec<u8> {
    let state = DaemonState::try_new().expect("daemon state");
    let (server, mut client) = UnixStream::pair().expect("socketpair");
    let admission = Admission::new(limits);
    let serving = tokio::spawn(serve_connection(server, state, admission));

    use tokio::io::AsyncWriteExt;
    client.write_all(request).await.expect("write");
    client.flush().await.expect("flush");

    let body = read_response_async(&mut client, MAX_RESPONSE_BYTES)
        .await
        .expect("response frame");
    serving.await.expect("serving task");
    body
}

#[tokio::test]
async fn a_health_request_is_served_and_its_id_is_echoed() {
    let _data = DataDir::new();
    let request = encode_frame(
        KIND_REQUEST,
        &envelope("r-health", "daemon.health", json!({})),
    );
    let response = exchange(&request, Limits::default()).await;
    assert_eq!(response["v"], json!(PROTOCOL_VERSION));
    assert_eq!(response["id"], json!("r-health"));
    assert_eq!(response["ok"], json!(true));
    assert_eq!(response["result"]["daemon"], json!("clawd"));
}

#[tokio::test]
async fn an_unknown_command_is_refused_over_the_socket() {
    let _data = DataDir::new();
    let request = encode_frame(
        KIND_REQUEST,
        &envelope("r-probe", "vendor.debug.dump", json!({})),
    );
    let response = exchange(&request, Limits::default()).await;
    assert_eq!(response["ok"], json!(false));
    assert_eq!(response["id"], json!("r-probe"));
    assert_eq!(response["error"]["code"], json!("unknown_command"));
    assert_eq!(
        response["error"]["message"],
        json!(Fault::UnknownCommand.message())
    );
    let rendered = response.to_string();
    assert!(
        !rendered.contains("vendor.debug.dump"),
        "the caller's route name must not be echoed: {rendered}"
    );
}

#[tokio::test]
async fn an_undeclared_field_is_refused_over_the_socket() {
    let _data = DataDir::new();
    let request = encode_frame(
        KIND_REQUEST,
        &envelope("r-extra", "daemon.health", json!({"impersonate_uid": 0})),
    );
    let response = exchange(&request, Limits::default()).await;
    assert_eq!(response["ok"], json!(false));
    assert_eq!(response["error"]["code"], json!("invalid_request"));
    assert_eq!(
        response["error"]["message"],
        json!(Fault::InvalidParams.message())
    );
}

#[tokio::test]
async fn an_oversized_frame_is_refused_over_the_socket() {
    let _data = DataDir::new();
    let mut request = Vec::new();
    request.extend_from_slice(b"CBK1");
    request.push(KIND_REQUEST);
    request.push(0);
    request.extend_from_slice(&u32::MAX.to_be_bytes());
    let response = exchange(&request, Limits::default()).await;
    assert_eq!(response["ok"], json!(false));
    assert_eq!(response["error"]["code"], json!("protocol_error"));
    assert_eq!(
        response["error"]["message"],
        json!(Fault::FrameTooLarge.message())
    );
}

#[tokio::test]
async fn a_pipelined_second_frame_refuses_the_whole_exchange() {
    let _data = DataDir::new();
    let one = encode_frame(KIND_REQUEST, &envelope("r-1", "daemon.health", json!({})));
    let mut request = one.clone();
    request.extend_from_slice(&one);
    let response = exchange(&request, Limits::default()).await;
    assert_eq!(response["ok"], json!(false));
    assert_eq!(response["error"]["code"], json!("protocol_error"));
    assert_eq!(
        response["error"]["message"],
        json!(Fault::ExtraFrame.message())
    );
}

#[tokio::test]
async fn a_pre_v1_client_gets_one_actionable_line_and_nothing_else() {
    let _data = DataDir::new();
    let raw = exchange_legacy(b"{\"command\":\"daemon.health\",\"params\":{}}\n").await;
    assert!(raw.ends_with(b"\n"));
    let parsed: Value = serde_json::from_slice(&raw[..raw.len() - 1]).expect("json line");
    assert_eq!(parsed["ok"], json!(false));
    assert_eq!(
        parsed["error"]["message"],
        json!(Fault::UnsupportedFrame.message())
    );
    assert!(
        parsed.get("result").is_none(),
        "a pre-v1 request must never be served"
    );
}

async fn exchange_legacy(request: &[u8]) -> Vec<u8> {
    let state = DaemonState::try_new().expect("daemon state");
    let (server, mut client) = UnixStream::pair().expect("socketpair");
    let serving = tokio::spawn(serve_connection(
        server,
        state,
        Admission::new(Limits::default()),
    ));

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    client.write_all(request).await.expect("write");
    client.flush().await.expect("flush");
    let mut buf = Vec::new();
    client.read_to_end(&mut buf).await.expect("read");
    serving.await.expect("serving task");
    buf
}

#[tokio::test]
async fn a_client_that_never_finishes_its_frame_is_dropped_at_the_read_deadline() {
    let _data = DataDir::new();
    let state = DaemonState::try_new().expect("daemon state");
    let (server, mut client) = UnixStream::pair().expect("socketpair");
    let limits = Limits {
        read_deadline: std::time::Duration::from_millis(100),
        ..Limits::default()
    };
    let serving = tokio::spawn(serve_connection(server, state, Admission::new(limits)));

    use tokio::io::AsyncWriteExt;
    // One byte of a header, then silence: the classic slowloris.
    client.write_all(b"C").await.expect("write");
    client.flush().await.expect("flush");

    let response = read_response_async(&mut client, MAX_RESPONSE_BYTES)
        .await
        .expect("a refusal frame");
    let parsed: Value = serde_json::from_slice(&response).expect("json");
    assert_eq!(
        parsed["error"]["message"],
        json!(Fault::ReadTimeout.message())
    );
    serving.await.expect("serving task");
}

#[tokio::test]
async fn a_route_that_this_peer_cannot_reach_is_refused_over_the_socket() {
    if unsafe { libc::geteuid() } == 0 {
        // The access class is derived from the running uid; a root test
        // runner would legitimately be allowed through.
        return;
    }
    let _data = DataDir::new();
    let request = encode_frame(
        KIND_REQUEST,
        &envelope("r-root", "context.update", json!({"source": "probe"})),
    );
    let response = exchange(&request, Limits::default()).await;
    assert_eq!(response["ok"], json!(false));
    assert_eq!(response["error"]["code"], json!("not_authorized"));
    assert_eq!(
        response["error"]["message"],
        json!(Fault::NotAuthorized.message())
    );
}

#[test]
fn every_route_is_reachable_by_name_and_declares_an_access_class() {
    for route in ROUTES {
        assert_eq!(Command::parse(route.name), Some(route.command));
        assert!(matches!(
            route.access,
            Access::User | Access::Root | Access::PrivateTaskHost
        ));
        assert!(matches!(route.kind, Kind::Query | Kind::Mutation));
    }
}

#[tokio::test]
async fn socket_preparation_preserves_operation_and_io_source() {
    let directory = tempfile::tempdir().unwrap();
    let parent_file = directory.path().join("not-a-directory");
    std::fs::write(&parent_file, "occupied").unwrap();
    let socket = parent_file.join("clawd.sock");

    let error = prepare_socket(&socket).await.unwrap_err();

    assert_eq!(error.operation(), "socket.create_parent");
    assert!(std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<std::io::Error>())
        .is_some());
}
