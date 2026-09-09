use super::*;
use crate::clawd::authority::authorize_relayed;
use crate::clawd::protocol::{encode_response, Response};
use crate::clawd::transport::{frame::PeerStream, peer, ReadOutcome};
use crate::worker::{derive, BrokerAuthority, Mount, MountClass, WorkerLaunch};
use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

type Lines = tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>;

fn input(name: &str) -> PathBuf {
    std::fs::canonicalize(std::env::var_os(name).unwrap_or_else(|| panic!("set {name}"))).unwrap()
}

async fn line(reader: &mut Lines) -> String {
    tokio::time::timeout(Duration::from_secs(15), reader.next_line())
        .await
        .expect("native fixture deadline")
        .unwrap()
        .expect("native fixture exited")
}

async fn rpc(
    writer: &mut tokio::process::ChildStdin,
    reader: &mut Lines,
    id: u64,
    method: &str,
    params: Value,
) -> Value {
    writer
        .write_all(
            format!(
                "{}\n",
                json!({
                    "jsonrpc":"2.0","id":id,"method":method,"params":params,
                })
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let response: Value = serde_json::from_str(&line(reader).await).unwrap();
    assert_eq!(response["id"], id, "{response}");
    response
}

fn call(name: &str, arguments: Value) -> Value {
    json!({
        "name":name,"arguments":arguments,
        "_meta":{"claw-os.dev/call-context":{
            "wire_version":1,"call_id":format!("call-{}", uuid::Uuid::new_v4().simple()),"trace_id":"fixture",
            "session_id":"forged-correlation-not-authority","task_id":"forged-task",
            "deadline_unix_ms":until(),
            "caller":{"kind":"system-agent","id":"fixture","owner_uid":23456},
        }}
    })
}

fn result(value: Value) -> Value {
    assert!(
        !value["result"]["isError"].as_bool().unwrap_or(false),
        "{value}"
    );
    serde_json::from_str(value["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

async fn rendered(reader: &mut Lines) -> Value {
    loop {
        let value: Value = serde_json::from_str(&line(reader).await).unwrap();
        if !value["rendered"].is_null() {
            return value["rendered"].clone();
        }
    }
}

async fn wait_state(
    service: &dyn NotificationService,
    uid: u32,
    id: &str,
    state: NotificationState,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while service.get(uid, id).unwrap().state != state {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("durable notification transition");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires native Notifications, native presentation fixture, desktop delivery fixture and cos"]
async fn notifications_actual_native_worker_durable_delivery_and_owner_bound_ui() {
    let _lock = crate::test_env::lock_env();
    let native = input("COS_NOTIFICATIONS_BINARY");
    let manifest = input("COS_NOTIFICATIONS_MANIFEST");
    let presenter = input("COS_NOTIFICATIONS_PRESENTER");
    let bridge = input("COS_NOTIFICATIONS_DELIVERY");
    let cos = input("COS_NOTIFICATIONS_COS");
    let root = tempfile::Builder::new()
        .prefix("nf-")
        .tempdir_in(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../build"))
        .unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let data = root_path.join("data");
    let app = root_path.join("app");
    std::fs::create_dir(&app).unwrap();
    std::fs::copy(manifest, app.join("app.json")).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", &data);
    let _runtime = crate::test_env::TestEnvVarGuard::set("COS_RUNTIME_DIR", &root_path);
    let _xdg = crate::test_env::TestEnvVarGuard::set("XDG_RUNTIME_DIR", &root_path);
    let _proxy = crate::test_env::TestEnvVarGuard::remove(
        crate::extension_host::protocol::BROKER_SOCKET_ENV,
    );
    let uid = unsafe { libc::geteuid() };
    let session = format!("notification-{}", uuid::Uuid::new_v4().simple());
    let caps = crate::caps::CapSet::from_caps([Cap::unscoped(Verb::UI_NOTIFY)]);
    let slot = crate::worker::relay_slot();
    let mut policy = derive::app_session(derive::AppSessionInput {
        app_id: APP,
        app_dir: &app,
        program: native,
        argv: vec![],
        caps: &caps,
        authorized_mounts: &[],
        lifetime: derive::SessionLifetime::Reusable,
        session_id: &session,
        data_dir: data.to_str().unwrap(),
        apps_dir: root_path.to_str().unwrap(),
        extra_env: BTreeMap::from([
            ("COS_MCP_SERVER".into(), "1".into()),
            (
                "COS_APP_MANIFEST".into(),
                app.join("app.json").to_str().unwrap().into(),
            ),
        ]),
        package_identity: None,
        pinned_entries: vec![],
        transports: &[],
    })
    .unwrap();
    policy.env.remove("CLAW_COS_BIN");
    policy
        .env
        .insert("TMPDIR".into(), policy.workdir.to_str().unwrap().into());
    policy
        .mounts
        .retain(|mount| mount.target != std::path::Path::new("/usr/local/bin/cos"));
    policy.mounts.push(Mount::read_only(
        cos,
        "/usr/local/bin/cos",
        MountClass::Runtime,
    ));
    assert!(policy
        .mounts
        .iter()
        .all(|mount| mount.class != MountClass::Display));
    assert!(!policy.env.contains_key("DBUS_SESSION_BUS_ADDRESS"));
    assert_eq!(policy.seccomp.as_str(), "strict");
    assert_eq!(policy.network.as_str(), "denied");
    let prepared = crate::worker::prepare(&WorkerLaunch::new(policy).with_authority(
        BrokerAuthority::new(&session, Some(APP.into()), caps.clone(), slot.clone()),
    ))
    .unwrap();
    let mut worker = tokio::process::Command::from(prepared.command)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut writer = worker.stdin.take().unwrap();
    let mut reader = tokio::io::BufReader::new(worker.stdout.take().unwrap()).lines();
    let (_, session_grant) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, worker.id().unwrap()).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(&session).with_app(Some(APP.into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps,
            lifetime: Duration::from_secs(120),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .unwrap();
    let (relay, relay_grant) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id()).unwrap(),
            binding: Binding::Process,
            subject: Subject::session(&session).with_app(Some(APP.into())),
            audience: AudienceSet::one(Audience::AppRelay),
            caps: crate::caps::CapSet::new(),
            lifetime: Duration::from_secs(120),
            uses: Uses::Unbounded,
            index_session: false,
        })
        .unwrap();
    crate::worker::install_relay(&slot, Some(relay.into_wire()));
    let listener = tokio::net::UnixListener::bind(root_path.join("clawd.sock")).unwrap();
    peer::enable_credential_passing(listener.as_raw_fd()).unwrap();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = PeerStream::new(stream).unwrap();
            let ReadOutcome::Frame(frame) = stream
                .read_request(crate::clawd::wire::MAX_REQUEST_BYTES)
                .await
                .unwrap()
            else {
                panic!("notification fixture frame")
            };
            let client = ClientIdentity::from_peer(peer::verify(frame.credentials).unwrap());
            assert_eq!(client.uid, Some(uid));
            let request: crate::clawd::wire::InboundRequest =
                serde_json::from_slice(&frame.body).unwrap();
            let command = Command::parse(request.command.as_str()).unwrap();
            let params = (command.route().decode)(request.params).unwrap();
            let value = if command == Command::AppSessionRelay {
                assert_eq!(params["command"], "system.notification.control");
                let route = Command::SystemNotificationControl.route();
                let body = (route.decode)(params["params"].clone()).unwrap();
                let authority = authorize_relayed(
                    params["handle"].as_str().unwrap(),
                    params["session_id"].as_str().unwrap(),
                    route.name,
                    &route.authority,
                    &body,
                    &client,
                )
                .await
                .unwrap()
                .unwrap();
                control(body, &client, &authority).map(|value| json!({"result":value}))
            } else {
                let value = match command {
                    Command::NotificationDeliveryClaim => {
                        crate::clawd::notifications::claim_deliveries(params, &client)
                    }
                    Command::NotificationDeliveryComplete => {
                        crate::clawd::notifications::complete_delivery(params, &client)
                    }
                    Command::NotificationSubscribe => {
                        crate::clawd::notifications::subscribe(params, &client).await
                    }
                    Command::NotificationAcknowledge => {
                        crate::clawd::notifications::acknowledge(params, &client)
                    }
                    Command::NotificationDismiss => {
                        crate::clawd::notifications::dismiss(params, &client)
                    }
                    _ => panic!("unexpected notification fixture route: {command}"),
                };
                value.map_err(BrokerError::from)
            };
            let response = match value {
                Ok(value) => Response::ok(request.id, value),
                Err(error) => Response::error(request.id, error.kind.code(), error.message),
            };
            stream
                .write_response(&encode_response(&response).unwrap())
                .await
                .unwrap();
        }
    });
    let address = format!("unix:path={}/bus", root_path.display());
    let mut bus = tokio::process::Command::new("dbus-daemon")
        .args([
            "--session",
            "--nofork",
            "--nopidfile",
            "--nosyslog",
            "--print-address=1",
        ])
        .arg(format!("--address={address}"))
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut bus_lines = tokio::io::BufReader::new(bus.stdout.take().unwrap()).lines();
    assert!(line(&mut bus_lines).await.starts_with(&address));
    let start = |binary: &PathBuf| {
        let mut command = tokio::process::Command::new(binary);
        command
            .env_clear()
            .env("DBUS_SESSION_BUS_ADDRESS", &address)
            .env("HOME", &root_path)
            .env("COS_RUNTIME_DIR", &root_path)
            .env("TMPDIR", &root_path)
            .env("XDG_CONFIG_HOME", root_path.join("config"))
            .env("XDG_CACHE_HOME", root_path.join("cache"))
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        command
    };
    let mut ui = start(&presenter).spawn().unwrap();
    let mut ui_input = ui.stdin.take().unwrap();
    let mut ui_output = tokio::io::BufReader::new(ui.stdout.take().unwrap()).lines();
    assert_eq!(line(&mut ui_output).await, "ready");
    let wrong_presenter = start(&bridge)
        .arg(input("COS_NOTIFICATIONS_COS"))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let refused = tokio::time::timeout(Duration::from_secs(5), wrong_presenter.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8(refused.stderr)
        .unwrap()
        .contains("owner's native presenter"));
    let mut delivery = start(&bridge).arg(&presenter).spawn().unwrap();
    let mut delivery_output = tokio::io::BufReader::new(delivery.stdout.take().unwrap()).lines();
    assert_eq!(line(&mut delivery_output).await, "ready");
    rpc(&mut writer, &mut reader, 1, "initialize", json!({
        "protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"notification-fixture","version":"1"},
    })).await;
    writer
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();
    let args = json!({"summary":"Durable native","body":"<b>plain</b>","app_name":"Display label",
        "icon":"notification-symbolic","expire_ms":0,"transient":true,"dedupe_key":"fixture"});
    let posted = result(
        rpc(
            &mut writer,
            &mut reader,
            2,
            "tools/call",
            call("notify.post", args.clone()),
        )
        .await,
    );
    let id = posted["id"].as_str().unwrap();
    let service = notifications::open_default().unwrap();
    let stored = service.get(uid, id).unwrap();
    assert_eq!(stored.source, SOURCE);
    assert_eq!(stored.session_id.as_deref(), Some(session.as_str()));
    assert_eq!(stored.task_id, None);
    assert!(service.list(23456, false, 10).unwrap().is_empty());
    assert_eq!(stored.body, "<b>plain</b>");
    let shown = rendered(&mut ui_output).await;
    let desktop_id = shown["id"].as_u64().unwrap() as u32;
    assert_eq!(shown["body"], "&lt;b&gt;plain&lt;/b&gt;");
    assert_eq!(shown["app_name"], "Display label");
    assert_eq!(shown["app_icon"], "notification-symbolic");
    assert_eq!(shown["expire_timeout"], 0);
    assert_eq!(
        service.get(uid, id).unwrap().state,
        NotificationState::Unread
    );
    let replay = result(
        rpc(
            &mut writer,
            &mut reader,
            3,
            "tools/call",
            call("notify.post", args),
        )
        .await,
    );
    assert_eq!(replay, posted);
    assert_eq!(rendered(&mut ui_output).await["id"], desktop_id);
    assert_eq!(service.get(uid, id).unwrap().occurrences, 2);

    let external = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    let proxy = zbus::Proxy::new(
        &external,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await
    .unwrap();
    let close: zbus::Result<()> = proxy.call("CloseNotification", &(desktop_id,)).await;
    assert!(
        close.is_err(),
        "another sender must not close the bridge's presentation"
    );
    let spoof: zbus::Result<u32> = proxy
        .call(
            "Notify",
            &(
                "Claw OS Agent",
                desktop_id,
                "com.clawos.Agent",
                "Forged",
                "Forged",
                Vec::<&str>::new(),
                std::collections::HashMap::<&str, zbus::zvariant::Value<'_>>::new(),
                0_i32,
            ),
        )
        .await;
    assert!(
        spoof.is_err(),
        "labels and guessed ids never replace another sender"
    );
    external
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "ActionInvoked",
            &(desktop_id, "default"),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        service.get(uid, id).unwrap().state,
        NotificationState::Unread
    );
    let foreign_id: u32 = proxy
        .call(
            "Notify",
            &(
                "External fixture",
                0_u32,
                "",
                "Freedesktop still works",
                "Untrusted sender",
                Vec::<&str>::new(),
                std::collections::HashMap::from([(
                    "x-claw-notification-id",
                    zbus::zvariant::Value::from(id),
                )]),
                0_i32,
            ),
        )
        .await
        .unwrap();
    assert_ne!(foreign_id, desktop_id);
    assert_eq!(rendered(&mut ui_output).await["id"], foreign_id);
    ui_input
        .write_all(format!("{}\n", json!({"action":"ack","id":foreign_id})).as_bytes())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        service.get(uid, id).unwrap().state,
        NotificationState::Unread,
        "even a genuine action on a foreign presentation cannot use forged durable-id hints"
    );
    let other = service
        .publish(
            uid,
            NotificationDraft::new("agent", "test", Severity::Info, "Other", "Body").activity(),
        )
        .unwrap();
    let denied = rpc(
        &mut writer,
        &mut reader,
        4,
        "tools/call",
        call("notify.close", json!({"id":other.id})),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    assert_eq!(
        service.get(uid, &other.id).unwrap().state,
        NotificationState::Unread
    );
    let invalid = rpc(
        &mut writer,
        &mut reader,
        5,
        "tools/call",
        call(
            "notify.post",
            json!({
                "summary":"Refused","icon":"/etc/shadow",
            }),
        ),
    )
    .await;
    assert_eq!(invalid["result"]["isError"], true);
    let closed = result(
        rpc(
            &mut writer,
            &mut reader,
            6,
            "tools/call",
            call("notify.close", json!({"id":id})),
        )
        .await,
    );
    assert_eq!(closed["state"], "dismissed");
    loop {
        let value: Value = serde_json::from_str(&line(&mut ui_output).await).unwrap();
        if value["closed"] == desktop_id {
            break;
        }
    }
    assert!(service.get(uid, id).unwrap().acknowledged_at_ms.is_none());
    let second = result(
        rpc(
            &mut writer,
            &mut reader,
            7,
            "tools/call",
            call(
                "notify.post",
                json!({
                    "summary":"Acknowledge","expire_ms":0,
                }),
            ),
        )
        .await,
    );
    let shown = rendered(&mut ui_output).await;
    ui_input
        .write_all(format!("{}\n", json!({"action":"ack","id":shown["id"]})).as_bytes())
        .await
        .unwrap();
    wait_state(
        &service,
        uid,
        second["id"].as_str().unwrap(),
        NotificationState::Acknowledged,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        service
            .get(uid, second["id"].as_str().unwrap())
            .unwrap()
            .state,
        NotificationState::Acknowledged
    );
    let third = result(
        rpc(
            &mut writer,
            &mut reader,
            8,
            "tools/call",
            call(
                "notify.post",
                json!({
                    "summary":"Dismiss","expire_ms":0,
                }),
            ),
        )
        .await,
    );
    let shown = rendered(&mut ui_output).await;
    ui_input
        .write_all(format!("{}\n", json!({"action":"dismiss","id":shown["id"]})).as_bytes())
        .await
        .unwrap();
    wait_state(
        &service,
        uid,
        third["id"].as_str().unwrap(),
        NotificationState::Dismissed,
    )
    .await;
    for process in [&mut worker, &mut delivery, &mut ui, &mut bus] {
        process.kill().await.unwrap();
        process.wait().await.unwrap();
    }
    server.abort();
    let _ = server.await;
    authority().revoke(session_grant.id);
    authority().revoke(relay_grant.id);
    drop(prepared.resources);
}
