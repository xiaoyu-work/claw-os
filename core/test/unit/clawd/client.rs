use super::*;

#[cfg(target_os = "linux")]
#[test]
fn private_gui_provider_transport_cannot_be_used_for_root_control_commands() {
    for command in [
        super::super::routes::Command::SystemReviewShow,
        super::super::routes::Command::SystemReviewDecide,
        super::super::routes::Command::AppSessionRegister,
        super::super::routes::Command::AppDirectoryInstall,
    ] {
        let error = request_gui_resource_blocking(Request::build(command, serde_json::json!({}))).unwrap_err();
        assert!(!error.may_have_dispatched(), "{command}");
        assert!(requires_root_peer(command), "{command}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn system_review_refuses_a_user_owned_broker_before_sending_any_data() {
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("fake-review.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let error = request_blocking(&socket, Request::build(
        super::super::routes::Command::SystemReviewPending,
        serde_json::json!({"limit": 100}),
    )).unwrap_err();
    assert!(!error.may_have_dispatched());
    assert!(error.to_string().contains("root OS broker"));
    assert!(server.join().unwrap().is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn app_registration_refuses_a_user_owned_broker_before_sending_any_data() {
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("fake-registration.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let error = request_blocking(
        &socket,
        Request::build(
            super::super::routes::Command::AppSessionRegister,
            serde_json::json!({"app_id": "fixture", "kind": "mcp", "tool": "fixture.probe"}),
        ),
    )
    .unwrap_err();
    assert!(!error.may_have_dispatched());
    assert!(error.to_string().contains("root OS broker"));
    assert!(server.join().unwrap().is_empty());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn async_app_registration_refuses_a_user_owned_broker_before_sending_data() {
    use tokio::io::AsyncReadExt;

    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("fake-async-registration.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        bytes
    });
    let error = request(
        &socket,
        Request::build(
            super::super::routes::Command::AppSessionRegister,
            serde_json::json!({"app_id": "fixture", "kind": "mcp", "tool": "fixture.probe"}),
        ),
    )
    .await
    .unwrap_err();
    assert!(!error.may_have_dispatched());
    assert!(error.to_string().contains("root OS broker"));
    assert!(server.await.unwrap().is_empty());
}

#[test]
fn peer_authentication_includes_registration_without_changing_legacy_native_routes() {
    use super::super::routes::Command;

    assert!(requires_root_peer(Command::AppSessionRegister));
    assert!(requires_root_peer(Command::AppSessionCheckLive));
    assert!(requires_root_peer(Command::AppDirectoryInstall));
    assert!(requires_root_peer(Command::SystemReviewPrepare));
    assert!(requires_root_peer(Command::SystemReviewShow));
    assert!(requires_root_peer(Command::SystemReviewPending));
    assert!(requires_root_peer(Command::SystemReviewDecide));
    assert!(requires_root_peer(Command::SystemReviewConsume));
    assert!(requires_root_peer(Command::SystemReviewCancel));
    assert!(requires_root_peer(Command::SystemMailControl));
    assert!(requires_root_peer(Command::SystemRegionalSettingsControl));
    assert!(!requires_root_peer(Command::SystemClipboardControl));
    assert!(!requires_root_peer(Command::DaemonHealth));
    assert!(!requires_root_peer(Command::PermissionStatus));
    assert!(!requires_root_peer(Command::AppSessionRegisterNative));
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn clipboard_typed_clients_use_the_launcher_owned_worker_proxy_and_preserve_root_denials() {
    use crate::caps::{Cap, CapSet, Scope, Verb};
    use crate::clawd::protocol::encode_response;
    use crate::clawd::routes::Command;
    use crate::clawd::transport::{frame::PeerStream, peer, ReadOutcome};
    use crate::worker::{broker::BrokerEndpoint, BrokerAuthority};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let _lock = crate::test_env::lock_env();
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let _runtime =
        crate::test_env::TestEnvVarGuard::set("COS_RUNTIME_DIR", directory.path());
    let _data =
        crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", directory.path().join("data"));
    let _proxy =
        crate::test_env::TestEnvVarGuard::remove(crate::extension_host::protocol::BROKER_SOCKET_ENV);
    let uid = unsafe { libc::geteuid() };
    let listener = tokio::net::UnixListener::bind(directory.path().join("clawd.sock")).unwrap();
    peer::enable_credential_passing(listener.as_raw_fd()).unwrap();
    let daemon = tokio::spawn(async move {
        for allowed in [true, false] {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = PeerStream::new(stream).unwrap();
            let ReadOutcome::Frame(frame) = stream
                .read_request(crate::clawd::wire::MAX_REQUEST_BYTES)
                .await
                .unwrap()
            else {
                panic!("framed Clipboard relay expected");
            };
            assert_eq!(frame.credentials.uid, uid);
            let request: crate::clawd::wire::InboundRequest =
                serde_json::from_slice(&frame.body).unwrap();
            assert_eq!(request.command.as_str(), Command::AppSessionRelay.as_str());
            assert_eq!(request.params["session_id"], "clipboard-private-client");
            assert_eq!(request.params["handle"], "launcher-only-clipboard-relay");
            assert_eq!(request.params["command"], "system.clipboard.control");
            let params =
                (Command::SystemClipboardControl.route().decode)(request.params["params"].clone())
                    .unwrap();
            crate::clawd::clipboard::validate_history_request(&params).unwrap();
            assert_eq!(params["session"], "clipboard-private-client");
            assert_eq!(params["action"], "history-list");
            assert!(params.get("owner_uid").is_none());
            let response = if allowed {
                Response::ok(request.id, serde_json::json!({"result":{"entries":[]}}))
            } else {
                Response::error(request.id, "not_authorized", "synthetic Root history denial")
            };
            stream
                .write_response(&encode_response(&response).unwrap())
                .await
                .unwrap();
        }
    });

    let relay = crate::worker::relay_slot();
    crate::worker::install_relay(&relay, Some("launcher-only-clipboard-relay".into()));
    let authority = BrokerAuthority::new(
        "clipboard-private-client",
        Some("panel-clipboard".into()),
        CapSet::from_caps([Cap::new(Verb::CLIPBOARD_READ, Scope::name("history"))]),
        relay,
    );
    let endpoint =
        BrokerEndpoint::start(directory.path().join("worker.sock"), authority, uid).unwrap();
    let metadata = std::fs::metadata(endpoint.socket_path()).unwrap();
    assert_eq!(metadata.uid(), uid);
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    let probe = UnixStream::connect(endpoint.socket_path()).await.unwrap();
    assert_eq!(probe.peer_cred().unwrap().uid(), uid);
    drop(probe);

    let params = serde_json::json!({
        "session":"clipboard-private-client", "action":"history-list",
        "deadline_unix_ms":u64::MAX
    });
    let socket = endpoint.socket_path().to_path_buf();
    let first = Request::build(Command::SystemClipboardControl, params.clone());
    let allowed = tokio::task::spawn_blocking(move || request_blocking(socket, first))
        .await
        .unwrap()
        .unwrap();
    assert!(allowed.ok, "{allowed:?}");
    assert_eq!(allowed.result.unwrap(), serde_json::json!({"entries":[]}));

    let denied = request(
        endpoint.socket_path(),
        Request::build(Command::SystemClipboardControl, params),
    )
    .await
    .unwrap();
    assert!(!denied.ok);
    let error = denied.error.unwrap();
    assert_eq!(error.code, "not_authorized");
    assert_eq!(error.message, "synthetic Root history denial");
    assert!(!error.message.contains("launcher-only-clipboard-relay"));
    daemon.await.unwrap();
}

#[cfg(unix)]
#[test]
fn connection_failure_is_known_to_be_pre_dispatch() {
    let directory = tempfile::tempdir().expect("tempdir");
    let request = Request::build(
        super::super::routes::Command::DaemonHealth,
        serde_json::json!({}),
    );
    let error = request_blocking(directory.path().join("missing.sock"), request)
        .expect_err("missing socket must fail");

    assert!(!error.may_have_dispatched());
}

#[cfg(unix)]
#[test]
fn response_failure_is_indeterminate_for_a_mutation_caller() {
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    let directory = tempfile::tempdir().expect("tempdir");
    let socket = directory.path().join("broker.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut header = [0_u8; super::super::wire::HEADER_BYTES];
        stream.read_exact(&mut header).expect("request header");
        let length = super::super::transport::frame::parse_header(
            &header,
            super::super::wire::KIND_REQUEST,
            super::super::wire::MAX_REQUEST_BYTES,
        )
        .expect("request frame");
        let mut body = vec![0_u8; length];
        stream.read_exact(&mut body).expect("request body");
    });
    let request = Request::build(
        super::super::routes::Command::DaemonHealth,
        serde_json::json!({}),
    );
    let error = request_blocking(&socket, request).expect_err("closed response must fail");
    server.join().expect("server");

    assert!(error.may_have_dispatched());
}
