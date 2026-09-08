use super::*;

use crate::caps::{Cap, CapSet, Scope, Verb};

fn authority(caps: Vec<Cap>) -> BrokerAuthority {
    // No relay handle: this is the shape a launch has before its
    // session is bound, and every broker route must be refused.
    BrokerAuthority::new(
        "app-test",
        Some("fs".to_string()),
        CapSet::from_caps(caps),
        crate::worker::relay_slot(),
    )
}

fn relaying_authority(caps: Vec<Cap>) -> BrokerAuthority {
    let slot = crate::worker::relay_slot();
    crate::worker::install_relay(&slot, Some("relay-handle".to_string()));
    BrokerAuthority::new(
        "app-test",
        Some("fs".to_string()),
        CapSet::from_caps(caps),
        slot,
    )
}

#[test]
fn identity_and_consent_routes_are_always_refused() {
    let authority = relaying_authority(vec![Cap::new(Verb::SYS_OBSERVE, Scope::Wild)]);
    for command in Command::ALL.iter().copied().filter(|command| {
        let name = command.as_str();
        name.starts_with("app_session.")
            || name.starts_with("mcp_session.")
            || name.starts_with("permission.")
            || name.starts_with("journal.")
            || name.starts_with("scheduler.")
    }) {
        let error = admit(command, &authority).unwrap_err();
        assert!(error.contains("control route"), "{command:?}: {error}");
    }
}

#[test]
fn a_launch_with_no_relay_grant_reaches_no_route_at_all() {
    let authority = authority(vec![Cap::new(Verb::NET_MANAGE, Scope::Wild)]);
    let command = Command::parse("system.network.control").expect("network route");
    let error = admit(command, &authority).unwrap_err();
    assert!(error.contains("no relay authority"), "{error}");
}

#[test]
fn a_route_family_the_launch_cannot_justify_is_refused() {
    let authority = relaying_authority(vec![Cap::new(Verb::FS_READ, Scope::path("/tmp/**"))]);
    for command in Command::ALL.iter().copied() {
        if command == Command::SystemFilesystemRead {
            admit(command, &authority).expect("fs.read admits only the filesystem read provider");
            continue;
        }
        assert!(
            admit(command, &authority).is_err(),
            "{command:?} admitted on fs.read alone"
        );
    }
}

#[test]
fn filesystem_write_admission_does_not_follow_read_authority() {
    let read = relaying_authority(vec![Cap::new(Verb::FS_READ, Scope::path("/work/file"))]);
    assert!(admit(Command::SystemFilesystemWrite, &read).is_err());
    let write = relaying_authority(vec![Cap::new(Verb::FS_WRITE, Scope::path("/work/file"))]);
    admit(Command::SystemFilesystemWrite, &write).unwrap();
    assert!(admit(Command::SystemFilesystemRead, &write).is_err());
}

#[test]
fn every_admissible_route_is_explicitly_mapped() {
    // A route with no rule is refused, so a newly added `clawd` route
    // is closed to workers until somebody decides otherwise.
    let wide = relaying_authority(vec![Cap::new(Verb::SYS_OBSERVE, Scope::Wild)]);
    let unmapped: Vec<&str> = Command::ALL
        .iter()
        .copied()
        .filter(|command| !is_forbidden(*command) && required_verbs(*command).is_empty())
        .map(Command::as_str)
        .collect();
    for name in &unmapped {
        let command = Command::parse(name).expect("known route");
        let error = admit(command, &wide).unwrap_err();
        assert!(error.contains("no admission rule"), "{name}: {error}");
    }
}
#[test]
fn ai_chat_admission_is_explicit_and_cannot_admit_app_invocation() {
    for verb in [Verb::AI_CHAT, Verb::AI_CHAT_UNTRUSTED] {
        let granted = relaying_authority(vec![Cap::new(verb, Scope::name("model"))]);
        admit(Command::AiChat, &granted).unwrap();
        assert!(admit(Command::AppServiceCliCall, &granted).is_err());
        assert!(admit(Command::AppSessionRegister, &granted).is_err());
    }

    let read = relaying_authority(vec![Cap::new(Verb::FS_READ, Scope::Wild)]);
    assert!(admit(Command::AiChat, &read).is_err());
}

#[test]
fn settings_permission_management_relay_is_not_approval_authority() {
    let granted = relaying_authority(vec![Cap::new(Verb::SYS_PERMISSIONS, Scope::name("manage"))]);
    admit(Command::SystemAppPermissions, &granted).unwrap();
    for command in [Command::PermissionApps, Command::PermissionDecide, Command::PermissionRevoke] {
        assert!(admit(command, &granted).is_err());
    }
    let observation = relaying_authority(vec![Cap::new(Verb::SYS_OBSERVE, Scope::Wild)]);
    assert!(admit(Command::SystemAppPermissions, &observation).is_err());
}

#[test]
fn a_launch_holding_the_family_verb_is_admitted() {
    let granted = relaying_authority(vec![Cap::new(Verb::NET_MANAGE, Scope::Wild)]);
    let command = Command::parse("system.network.control").expect("network route");
    admit(command, &granted).expect("net.manage justifies the network route");

    let unrelated = relaying_authority(vec![Cap::new(Verb::SYS_PACKAGE, Scope::Wild)]);
    assert!(admit(command, &unrelated).is_err());
}

#[test]
fn browser_route_requires_one_of_the_browser_capability_families() {
    let command = Command::SystemBrowserControl;
    let granted = relaying_authority(vec![Cap::new(
        Verb::BROWSER_DOM_READ,
        Scope::host("example.com:443"),
    )]);
    admit(command, &granted).expect("browser.dom.read justifies the browser route");

    let unrelated = relaying_authority(vec![Cap::new(Verb::NET_DIAL, Scope::Wild)]);
    assert!(admit(command, &unrelated).is_err());
}

#[test]
fn network_diagnostic_route_requires_one_of_its_capability_families() {
    let command = Command::SystemNetworkDiagnose;
    for cap in [
        Cap::new(Verb::SYS_OBSERVE, Scope::name("network")),
        Cap::new(Verb::NET_RESOLVE, Scope::host("example.com")),
        Cap::new(Verb::NET_PROBE, Scope::host("example.com:443")),
    ] {
        let granted = relaying_authority(vec![cap]);
        admit(command, &granted).expect("network diagnostic capability justifies the route");
    }

    let unrelated = relaying_authority(vec![Cap::new(Verb::NET_MANAGE, Scope::Wild)]);
    assert!(admit(command, &unrelated).is_err());
}

#[test]
fn admission_and_policy_read_the_capability_set_live() {
    // The registry row is absent in a unit test, so the fallback is
    // what is seen. The assertion is that `live_caps()` — a read, not a
    // frozen field — is what both paths consult.
    let empty = relaying_authority(Vec::new());
    assert!(empty.live_caps().is_empty());
    let granted = relaying_authority(vec![Cap::new(Verb::SYS_PACKAGE, Scope::Wild)]);
    assert!(granted
        .live_caps()
        .covers(&Cap::new(Verb::SYS_PACKAGE, Scope::Wild)));
}

#[test]
fn policy_check_answers_from_the_launch_capability_set() {
    let authority = relaying_authority(vec![Cap::new(Verb::FS_READ, Scope::path("/srv/data/**"))]);

    let allowed = policy_check(
        &authority,
        &serde_json::json!({
            "verb": "fs.read",
            "scope": { "kind": "path", "value": "/srv/data/report.csv" },
        }),
    );
    assert_eq!(allowed["decision"], "allow");
    assert_eq!(allowed["session"], "app-test");

    let denied = policy_check(
        &authority,
        &serde_json::json!({
            "verb": "fs.read",
            "scope": { "kind": "path", "value": "/etc/shadow" },
        }),
    );
    assert_eq!(denied["decision"], "deny");

    let wrong_verb = policy_check(
        &authority,
        &serde_json::json!({
            "verb": "fs.write",
            "scope": { "kind": "path", "value": "/srv/data/report.csv" },
        }),
    );
    assert_eq!(wrong_verb["decision"], "deny");
}

#[test]
fn an_unknown_verb_is_denied_rather_than_ignored() {
    let authority = relaying_authority(vec![Cap::new(Verb::FS_READ, Scope::Wild)]);
    let answer = policy_check(&authority, &serde_json::json!({ "verb": "fs.teleport" }));
    assert_eq!(answer["decision"], "deny");
    assert_eq!(answer["reason"], "unknown-verb");
}

#[test]
fn a_missing_scope_is_treated_as_the_widest_request() {
    let narrow = relaying_authority(vec![Cap::new(Verb::FS_READ, Scope::path("/srv/**"))]);
    let answer = policy_check(&narrow, &serde_json::json!({ "verb": "fs.read" }));
    assert_eq!(answer["decision"], "deny");

    let wide = relaying_authority(vec![Cap::new(Verb::FS_READ, Scope::Wild)]);
    let answer = policy_check(&wide, &serde_json::json!({ "verb": "fs.read" }));
    assert_eq!(answer["decision"], "allow");
}

#[cfg(target_os = "linux")]
#[test]
fn the_endpoint_answers_a_policy_check_and_refuses_identity_control() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("broker.sock");
    let uid = unsafe { libc::geteuid() };
    let endpoint = BrokerEndpoint::start(
        socket.clone(),
        relaying_authority(vec![Cap::new(Verb::FS_READ, Scope::path("/srv/**"))]),
        uid,
    )
    .expect("start endpoint");

    let ask = |command: &str, params: serde_json::Value| -> serde_json::Value {
        let mut stream = UnixStream::connect(endpoint.socket_path()).expect("connect");
        let body = serde_json::json!({
            "v": crate::clawd::wire::PROTOCOL_VERSION,
            "id": "test",
            "command": command,
            "params": params,
        });
        let encoded = serde_json::to_vec(&body).unwrap();
        stream
            .write_all(&crate::clawd::transport::frame::encode_frame(
                crate::clawd::wire::KIND_REQUEST,
                &encoded,
            ))
            .expect("write");
        let mut header = [0_u8; crate::clawd::wire::HEADER_BYTES];
        stream.read_exact(&mut header).expect("header");
        let len = crate::clawd::transport::frame::parse_header(
            &header,
            crate::clawd::wire::KIND_RESPONSE,
            1024 * 1024,
        )
        .expect("parse");
        let mut response = vec![0_u8; len];
        stream.read_exact(&mut response).expect("body");
        serde_json::from_slice(&response).expect("json")
    };

    let allowed = ask(
        POLICY_CHECK_COMMAND,
        serde_json::json!({
            "verb": "fs.read",
            "scope": { "kind": "path", "value": "/srv/a.txt" },
        }),
    );
    assert_eq!(allowed["ok"], true);
    assert_eq!(allowed["v"], crate::clawd::wire::PROTOCOL_VERSION);
    assert_eq!(allowed["result"]["decision"], "allow");

    let refused = ask("app_session.register", serde_json::json!({}));
    assert_eq!(refused["ok"], false);
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("control route"),
        "{refused}"
    );

    let unknown = ask("not.a.route", serde_json::json!({}));
    assert_eq!(unknown["ok"], false);

    // Nothing the worker can see names the relay grant.
    let rendered = format!("{allowed}{refused}{unknown}");
    assert!(!rendered.contains("relay-handle"), "{rendered}");

    let facts = endpoint.facts();
    assert!(facts["served"].as_u64().unwrap_or(0) >= 1);
    assert!(facts["denied"].as_u64().unwrap_or(0) >= 2);
    assert!(!facts.to_string().contains("relay-handle"));
}

#[cfg(target_os = "linux")]
#[test]
fn the_standard_broker_client_accepts_worker_responses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("broker.sock");
    let uid = unsafe { libc::geteuid() };
    let endpoint =
        BrokerEndpoint::start(socket, authority(Vec::new()), uid).expect("start endpoint");
    let response = crate::clawd::client::request_blocking(
        endpoint.socket_path(),
        crate::clawd::protocol::Request::build(
            Command::SystemBrowserControl,
            serde_json::json!({}),
        ),
    )
    .expect("worker response must use the shared broker protocol");

    assert!(!response.ok);
    assert_eq!(response.error.expect("denial").code, "not_authorized");
}

#[test]
fn relay_error_bodies_keep_their_broker_classification() {
    let error = broker_error_from_body(crate::clawd::protocol::ErrorBody {
        code: "indeterminate".to_string(),
        message: "response lost after dispatch".to_string(),
        data: None,
        audit_class: None,
    });
    assert_eq!(
        error.kind,
        crate::clawd::protocol::BrokerErrorKind::Indeterminate
    );
    assert_eq!(error.message, "response lost after dispatch");
}

#[cfg(unix)]
#[test]
fn dropping_the_endpoint_removes_the_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("broker.sock");
    let uid = unsafe { libc::geteuid() };
    let endpoint = BrokerEndpoint::start(socket.clone(), authority(vec![]), uid).expect("start");
    assert!(socket.exists());
    drop(endpoint);
    assert!(!socket.exists(), "worker authority outlived its launch");
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn settings_request_crosses_the_private_worker_proxy_without_exposing_approval() {
    use std::os::fd::AsRawFd;
    use crate::clawd::transport::{frame::PeerStream, peer, ReadOutcome};
    use crate::clawd::protocol::{encode_response, Request, Response};
    let _lock = crate::test_env::lock_env();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../build");
    let dir = tempfile::tempdir_in(root).unwrap();
    let _runtime = crate::test_env::TestEnvVarGuard::set("COS_RUNTIME_DIR", dir.path());
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", dir.path().join("data"));
    let _proxy = crate::test_env::TestEnvVarGuard::remove(crate::extension_host::protocol::BROKER_SOCKET_ENV);
    let listener = tokio::net::UnixListener::bind(dir.path().join("clawd.sock")).unwrap();
    peer::enable_credential_passing(listener.as_raw_fd()).unwrap();
    let uid = unsafe { libc::geteuid() };
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = PeerStream::new(stream).unwrap();
        let ReadOutcome::Frame(frame) = stream.read_request(crate::clawd::wire::MAX_REQUEST_BYTES).await.unwrap() else {
            panic!("framed worker relay expected");
        };
        assert_eq!(frame.credentials.uid, uid);
        let request: crate::clawd::wire::InboundRequest = serde_json::from_slice(&frame.body).unwrap();
        assert_eq!(request.command.as_str(), Command::AppSessionRelay.as_str());
        assert_eq!(request.params["session_id"], "app-settings-test");
        assert_eq!(request.params["command"], "system.app-permissions");
        let params = (Command::SystemAppPermissions.route().decode)(request.params["params"].clone()).unwrap();
        assert_eq!(params["session"], "app-settings-test");
        assert_eq!(params["action"], "request");
        assert_eq!(params["app_id"], "audio-manager");
        assert!(params.get("owner_uid").is_none());
        let response = Response::ok(request.id, serde_json::json!({"result":{"status":"pending","id":"ap-fixture","enabled":false}}));
        stream.write_response(&encode_response(&response).unwrap()).await.unwrap();
    });
    let slot = crate::worker::relay_slot();
    crate::worker::install_relay(&slot, Some("daemon-only-handle".into()));
    let authority = BrokerAuthority::new("app-settings-test", Some("cosmic-settings".into()),
        CapSet::from_caps([Cap::new(Verb::SYS_PERMISSIONS, Scope::name("manage"))]), slot);
    let endpoint = BrokerEndpoint::start(dir.path().join("worker.sock"), authority, uid).unwrap();
    let socket = endpoint.socket_path().to_path_buf();
    let response = tokio::task::spawn_blocking(move || crate::clawd::client::request_blocking(
        socket, Request::build(Command::SystemAppPermissions, serde_json::json!({
            "session":"app-settings-test", "action":"request", "app_id":"audio-manager",
            "permission_id":"fixed-capability-key", "reason":"restore audio status"
        })))).await.unwrap().unwrap();
    assert!(response.ok, "{response:?}");
    assert_eq!(response.result.unwrap()["status"], "pending");
    server.await.unwrap();
    let socket = endpoint.socket_path().to_path_buf();
    let refused = tokio::task::spawn_blocking(move || crate::clawd::client::request_blocking(
        socket, Request::build(Command::PermissionDecide, serde_json::json!({
            "id":"ap-fixture", "decision":"approve", "owner_uid":uid
        })))).await.unwrap().unwrap();
    assert!(!refused.ok);
    assert_eq!(endpoint.facts()["relayed"], 1);
}
