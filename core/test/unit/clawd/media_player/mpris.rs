use super::*;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Bus {
    child: Child,
    _directory: tempfile::TempDir,
    address: String,
}

impl Bus {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("mb-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        let address = format!("unix:path={}/bus", directory.path().display());
        let mut child = Command::new("dbus-daemon")
            .args([
                "--session",
                "--nofork",
                "--nopidfile",
                "--nosyslog",
                "--print-address=1",
            ])
            .arg(format!("--address={address}"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("private dbus-daemon");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(line.starts_with(&address), "{line}");
        Self {
            child,
            _directory: directory,
            address,
        }
    }

    async fn connection(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn deadline() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 4000
}

fn fixture_executable() -> Executable {
    let path = std::env::current_exe().unwrap();
    let metadata = fs::metadata(&path).unwrap();
    // Only fixture code substitutes the installed native executable's inode.
    Executable {
        path,
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

async fn execute(
    connection: &Connection,
    uid: u32,
    executable: &Executable,
    action: MediaPlayerAction,
    deadline: u64,
) -> Result<Value, String> {
    super::execute(
        connection,
        uid,
        executable,
        action,
        deadline,
        std::future::ready(Ok(())),
    )
    .await
}

#[derive(Default)]
struct State {
    status: String,
    title: String,
    position: usize,
    calls: Vec<&'static str>,
}

struct Root {
    delay: Duration,
    desktop: &'static str,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    #[zbus(property)]
    async fn desktop_entry(&self) -> String {
        tokio::time::sleep(self.delay).await;
        self.desktop.into()
    }
}

struct Player(Arc<Mutex<State>>);

impl Player {
    fn action(&self, action: &'static str) {
        let mut state = self.0.lock().unwrap();
        state.calls.push(action);
        match action {
            "play" => state.status = "Playing".into(),
            "pause" => state.status = "Paused".into(),
            "stop" => {
                state.status = "Stopped".into();
                state.position = 0;
            }
            "next" => state.position += 1,
            "previous" => state.position = state.position.saturating_sub(1),
            "toggle" => {
                state.status = if state.status == "Playing" {
                    "Paused"
                } else {
                    "Playing"
                }
                .into()
            }
            _ => unreachable!(),
        }
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn play(&self) {
        self.action("play");
    }
    fn pause(&self) {
        self.action("pause");
    }
    fn stop(&self) {
        self.action("stop");
    }
    fn next(&self) {
        self.action("next");
    }
    fn previous(&self) {
        self.action("previous");
    }
    fn play_pause(&self) {
        self.action("toggle");
    }
    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.0.lock().unwrap().status.clone()
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        HashMap::from([
            (
                "xesam:title".into(),
                zbus::zvariant::Str::from(self.0.lock().unwrap().title.as_str()).into(),
            ),
            (
                "xesam:url".into(),
                zbus::zvariant::Str::from("file:///synthetic/song.ogg").into(),
            ),
            ("mpris:length".into(), 10_000_000_i64.into()),
        ])
    }
}

async fn player(bus: &Bus, name: &str, state: Arc<Mutex<State>>, delay: Duration) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(name)
        .unwrap()
        .serve_at(
            OBJECT,
            Root {
                delay,
                desktop: "com.clawos.Player",
            },
        )
        .unwrap()
        .serve_at(OBJECT, Player(state))
        .unwrap()
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn media_player_private_bus_controls_only_own_live_state_for_all_seven_tools() {
    let bus = Bus::new();
    let other = Arc::new(Mutex::new(State {
        status: "Playing".into(),
        title: "other player".into(),
        ..Default::default()
    }));
    let _other = player(
        &bus,
        "org.mpris.MediaPlayer2.vlc",
        other.clone(),
        Duration::ZERO,
    )
    .await;
    let state = Arc::new(Mutex::new(State {
        status: "Paused".into(),
        title: "visible UI title".into(),
        ..Default::default()
    }));
    let name = format!("{PREFIX}pid{}", std::process::id());
    let own = player(&bus, &name, state.clone(), Duration::ZERO).await;
    let connection = bus.connection().await;
    let exe = fixture_executable();
    let uid = unsafe { libc::geteuid() };
    let status = execute(
        &connection,
        uid,
        &exe,
        MediaPlayerAction::Status,
        deadline(),
    )
    .await
    .unwrap();
    assert_eq!(status["title"], "visible UI title");
    assert_eq!(status["url"], "file:///synthetic/song.ogg");
    assert_eq!(status["length_micros"], 10_000_000);
    for (action, expected, position) in [
        (MediaPlayerAction::Play, "Playing", 0),
        (MediaPlayerAction::Pause, "Paused", 0),
        (MediaPlayerAction::Toggle, "Playing", 0),
        (MediaPlayerAction::Next, "Playing", 1),
        (MediaPlayerAction::Previous, "Playing", 0),
        (MediaPlayerAction::Stop, "Stopped", 0),
    ] {
        assert_eq!(
            execute(&connection, uid, &exe, action, deadline())
                .await
                .unwrap(),
            json!({"ok":true})
        );
        let live = execute(
            &connection,
            uid,
            &exe,
            MediaPlayerAction::Status,
            deadline(),
        )
        .await
        .unwrap();
        assert_eq!(live["status"], expected);
        assert_eq!(state.lock().unwrap().position, position);
    }
    state.lock().unwrap().title = "changed in visible UI".into();
    assert_eq!(
        execute(
            &connection,
            uid,
            &exe,
            MediaPlayerAction::Status,
            deadline()
        )
        .await
        .unwrap()["title"],
        "changed in visible UI"
    );
    assert!(other.lock().unwrap().calls.is_empty());
    assert!(execute(
        &connection,
        uid + 1,
        &exe,
        MediaPlayerAction::Play,
        deadline()
    )
    .await
    .unwrap_err()
    .contains("owner"));
    let unrelated = Executable::installed(fs::canonicalize("/usr/bin/true").unwrap()).unwrap();
    assert!(execute(
        &connection,
        uid,
        &unrelated,
        MediaPlayerAction::Play,
        deadline()
    )
    .await
    .unwrap_err()
    .contains("installed native"));
    let second = player(
        &bus,
        &format!("{PREFIX}pid{}", std::process::id() + 1),
        state.clone(),
        Duration::ZERO,
    )
    .await;
    assert!(
        execute(&connection, uid, &exe, MediaPlayerAction::Play, deadline())
            .await
            .unwrap_err()
            .contains("ambiguous")
    );
    second.close().await.unwrap();
    own.close().await.unwrap();
    assert!(
        execute(&connection, uid, &exe, MediaPlayerAction::Play, deadline())
            .await
            .unwrap_err()
            .contains("no native")
    );
    assert!(other.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn media_player_independent_client_uses_live_authority_for_the_fixed_target() {
    use super::super::tests::decision;
    use super::super::{authorize, required_cap};
    use crate::clawd::authority::Uses;

    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State {
        status: "Paused".into(),
        title: "native UI state".into(),
        ..Default::default()
    }));
    let other = Arc::new(Mutex::new(State::default()));
    let _own = player(
        &bus,
        &format!("{PREFIX}pid{}", std::process::id()),
        state.clone(),
        Duration::ZERO,
    )
    .await;
    let _other = player(
        &bus,
        "org.mpris.MediaPlayer2.vlc",
        other.clone(),
        Duration::ZERO,
    )
    .await;
    let connection = bus.connection().await;
    let executable = fixture_executable();
    let uid = unsafe { libc::geteuid() };
    for action in [
        MediaPlayerAction::Status,
        MediaPlayerAction::Play,
        MediaPlayerAction::Pause,
        MediaPlayerAction::Stop,
        MediaPlayerAction::Next,
        MediaPlayerAction::Previous,
        MediaPlayerAction::Toggle,
    ] {
        let granted = decision(
            Some("independent-media-client"),
            vec![required_cap(action)],
            Uses::Unbounded,
        );
        let _authorized = authorize(&granted, action, uid).unwrap();
        let result = super::execute(
            &connection,
            uid,
            &executable,
            action,
            deadline(),
            async { authorize(&granted, action, uid).map(|_proof| ()) },
        )
        .await
        .unwrap();
        let _authorized = authorize(&granted, action, uid).unwrap();
        if action == MediaPlayerAction::Status {
            assert_eq!(result["title"], "native UI state");
        } else {
            assert_eq!(result, json!({"ok":true}));
        }
        assert_eq!(granted.app_id(), Some("independent-media-client"));
    }
    let calls = state.lock().unwrap().calls.clone();
    assert_eq!(calls, ["play", "pause", "stop", "next", "previous", "toggle"]);
    let cap = required_cap(MediaPlayerAction::Pause);
    let revoked = decision(
        Some("independent-media-client"),
        vec![cap.clone()],
        Uses::Unbounded,
    );
    let _authorized = authorize(&revoked, MediaPlayerAction::Pause, uid).unwrap();
    let error = super::execute(
        &connection,
        uid,
        &executable,
        MediaPlayerAction::Pause,
        deadline(),
        async {
            crate::approvals::app_policy::revoke(uid, "independent-media-client", cap)?;
            authorize(&revoked, MediaPlayerAction::Pause, uid).map(|_proof| ())
        },
    )
    .await
    .unwrap_err();
    assert!(error.contains("revoked"), "{error}");
    assert_eq!(state.lock().unwrap().calls, calls);
    assert!(other.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn media_player_expired_and_stalled_discovery_cannot_dispatch_a_control() {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(State::default()));
    let _own = player(
        &bus,
        &format!("{PREFIX}pid{}", std::process::id()),
        state.clone(),
        Duration::from_millis(80),
    )
    .await;
    let connection = bus.connection().await;
    let exe = fixture_executable();
    let uid = unsafe { libc::geteuid() };
    assert!(execute(&connection, uid, &exe, MediaPlayerAction::Play, 1)
        .await
        .unwrap_err()
        .contains("deadline"));
    let short = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 30;
    assert!(
        execute(&connection, uid, &exe, MediaPlayerAction::Play, short)
            .await
            .unwrap_err()
            .contains("deadline")
    );
    assert!(state.lock().unwrap().calls.is_empty());
    assert!(super::execute(
        &connection,
        uid,
        &exe,
        MediaPlayerAction::Play,
        deadline(),
        std::future::ready(Err("cancelled or revoked".into()))
    )
    .await
    .unwrap_err()
    .contains("revoked"));
    assert!(state.lock().unwrap().calls.is_empty());
}

#[test]
fn media_player_names_metadata_and_executable_trust_fail_closed() {
    for suffix in [
        "pid0",
        "pid01",
        "pid-1",
        "pid999999999999",
        "vlc",
        "pid1.other",
    ] {
        assert!(select([format!("{PREFIX}{suffix}")]).is_err());
    }
    assert!(select(["org.mpris.MediaPlayer2.vlc".into()])
        .unwrap_err()
        .contains("no native"));
    let mut metadata = HashMap::new();
    metadata.insert("xesam:title".into(), 42_i64.into());
    assert!(status_value("Playing".into(), metadata).is_err());
    assert!(status_value("Fabricated".into(), HashMap::new()).is_err());
    assert!(
        Executable::installed(std::env::current_exe().unwrap()).is_err(),
        "a user build is not an installed product"
    );
    let own = fixture_executable();
    assert!(own
        .process(unsafe { libc::geteuid() } + 1, std::process::id())
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires separately built Player, native MPRIS fixture, cos and staged manifest"]
async fn media_player_actual_native_mcp_crosses_worker_relay() {
    use crate::caps::{CapSet, Scope, Verb};
    use crate::clawd::authority::{
        authority, authorize_relayed, Audience, AudienceSet, Binding, Issuance, Issuer, Principal,
        Subject, Uses,
    };
    use crate::clawd::client_identity::ClientIdentity;
    use crate::clawd::protocol::{encode_response, Response};
    use crate::clawd::routes::Command as Route;
    use crate::clawd::transport::{frame::PeerStream, peer, ReadOutcome};
    use crate::worker::{derive, BrokerAuthority, Mount, MountClass, WorkerLaunch};
    use std::collections::BTreeMap;
    use std::os::fd::AsRawFd;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    struct Native(Child);
    impl Drop for Native {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn input(name: &str) -> PathBuf {
        fs::canonicalize(std::env::var_os(name).unwrap_or_else(|| panic!("set {name}"))).unwrap()
    }
    fn ready(process: &mut Native) -> String {
        let mut value = String::new();
        BufReader::new(process.0.stdout.as_mut().unwrap())
            .read_line(&mut value)
            .unwrap();
        value.trim().to_string()
    }
    async fn rpc(
        writer: &mut tokio::process::ChildStdin,
        reader: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
        id: u64,
        method: &str,
        params: Value,
    ) -> Value {
        writer
            .write_all(
                format!(
                    "{}\n",
                    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let line = tokio::time::timeout(Duration::from_secs(10), reader.next_line())
            .await
            .expect("native MCP response deadline")
            .unwrap()
            .expect("native MCP exited");
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id, "{response}");
        response
    }
    fn call_params(action: &str) -> Value {
        json!({
            "name":format!("player.{action}"), "arguments":{},
            "_meta":{"claw-os.dev/call-context":{
                "wire_version":1,"call_id":format!("fixture-{action}"),"trace_id":"fixture",
                "session_id":"non-authoritative-metadata","task_id":"fixture",
                "deadline_unix_ms":deadline(),
                "caller":{"kind":"system-agent","id":"fixture","owner_uid":unsafe { libc::geteuid() }}
            }}
        })
    }
    fn result(reply: Value) -> Value {
        assert!(
            !reply["result"]["isError"].as_bool().unwrap_or(false),
            "{reply}"
        );
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    let _lock = crate::test_env::lock_env();
    let fixture = input("COS_MEDIA_PLAYER_MPRIS_FIXTURE");
    let native = input("COS_MEDIA_PLAYER_BINARY");
    let cos = input("COS_MEDIA_PLAYER_COS");
    let manifest = input("COS_MEDIA_PLAYER_MANIFEST");
    let directory = tempfile::Builder::new()
        .prefix("mp-")
        .tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("../build"))
        .unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    let data = root.join("data");
    let app = root.join("app");
    fs::create_dir(&app).unwrap();
    fs::copy(manifest, app.join("app.json")).unwrap();
    let _runtime = crate::test_env::TestEnvVarGuard::set("COS_RUNTIME_DIR", directory.path());
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", &data);
    let _xdg = crate::test_env::TestEnvVarGuard::set("XDG_RUNTIME_DIR", directory.path());
    let _proxy = crate::test_env::TestEnvVarGuard::remove(
        crate::extension_host::protocol::BROKER_SOCKET_ENV,
    );
    let bus = Bus::new();
    let start = |args: &[&str]| {
        Native(
            Command::new(&fixture)
                .args(args)
                .env_clear()
                .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
                .env("TMPDIR", directory.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        )
    };
    let mut other = start(&["--other"]);
    assert_eq!(ready(&mut other), "other-ready");
    let mut ui = start(&[]);
    assert_eq!(ready(&mut ui), format!("{PREFIX}pid{}", ui.0.id()));
    let metadata = fs::metadata(&fixture).unwrap();
    let executable = Executable {
        path: fixture,
        dev: metadata.dev(),
        ino: metadata.ino(),
    };
    let connection = bus.connection().await;
    let uid = unsafe { libc::geteuid() };
    let session = format!("media-{}", uuid::Uuid::new_v4().simple());
    let caps = CapSet::from_caps([
        super::super::required_cap(MediaPlayerAction::Status),
        super::super::required_cap(MediaPlayerAction::Play),
    ]);
    let slot = crate::worker::relay_slot();
    let mut policy = derive::app_session(derive::AppSessionInput {
        app_id: "cosmic-player",
        app_dir: &app,
        program: native,
        argv: vec![],
        caps: &caps,
        authorized_mounts: &[],
        lifetime: derive::SessionLifetime::Reusable,
        session_id: &session,
        data_dir: data.to_str().unwrap(),
        apps_dir: directory.path().to_str().unwrap(),
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
        .retain(|mount| mount.target != Path::new("/usr/local/bin/cos"));
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
    assert_eq!(policy.network.as_str(), "denied");
    assert_eq!(policy.seccomp.as_str(), "strict");
    let launch = WorkerLaunch::new(policy).with_authority(BrokerAuthority::new(
        &session,
        Some("cosmic-player".into()),
        caps.clone(),
        slot.clone(),
    ));
    let prepared = crate::worker::prepare(&launch).unwrap();
    let mut command = tokio::process::Command::from(prepared.command);
    let mut child = command
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut writer = child.stdin.take().unwrap();
    let mut reader = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let (_, session_grant) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, child.id().unwrap()).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(&session).with_app(Some("cosmic-player".into())),
            audience: AudienceSet::one(Audience::SystemService),
            caps,
            lifetime: Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .unwrap();
    let (relay, relay_grant) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id()).unwrap(),
            binding: Binding::Process,
            subject: Subject::session(&session).with_app(Some("cosmic-player".into())),
            audience: AudienceSet::one(Audience::AppRelay),
            caps: CapSet::new(),
            lifetime: Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: false,
        })
        .unwrap();
    crate::worker::install_relay(&slot, Some(relay.into_wire()));
    let listener = tokio::net::UnixListener::bind(directory.path().join("clawd.sock")).unwrap();
    peer::enable_credential_passing(listener.as_raw_fd()).unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..16 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = PeerStream::new(stream).unwrap();
            let ReadOutcome::Frame(frame) = stream
                .read_request(crate::clawd::wire::MAX_REQUEST_BYTES)
                .await
                .unwrap()
            else {
                panic!("worker relay frame")
            };
            let client = ClientIdentity::from_peer(peer::verify(frame.credentials).unwrap());
            assert_eq!(client.uid, Some(uid));
            let request: crate::clawd::wire::InboundRequest =
                serde_json::from_slice(&frame.body).unwrap();
            assert_eq!(request.command.as_str(), "app_session.relay");
            let relay = (Route::AppSessionRelay.route().decode)(request.params).unwrap();
            assert_eq!(relay["command"], "system.media-player.control");
            let route = Route::SystemMediaPlayerControl.route();
            let params = (route.decode)(relay["params"].clone()).unwrap();
            let authority = authorize_relayed(
                relay["handle"].as_str().unwrap(),
                relay["session_id"].as_str().unwrap(),
                route.name,
                &route.authority,
                &params,
                &client,
            )
            .await
            .unwrap()
            .unwrap();
            let request_body: crate::clawd::wire::requests::MediaPlayerControl =
                serde_json::from_value(params).unwrap();
            let value = async {
                let _grant = super::super::authorize(&authority, request_body.action, uid)?;
                let permission = async {
                    let _grant = super::super::authorize(&authority, request_body.action, uid)?;
                    Ok(())
                };
                let value = super::execute(
                    &connection,
                    uid,
                    &executable,
                    request_body.action,
                    request_body.deadline_unix_ms,
                    permission,
                )
                .await?;
                let _grant = super::super::authorize(&authority, request_body.action, uid)?;
                Ok::<_, String>(value)
            }
            .await;
            let response = match value {
                Ok(value) => Response::ok(request.id, json!({"result":value})),
                Err(error) => Response::error(request.id, "not_authorized", error),
            };
            stream
                .write_response(&encode_response(&response).unwrap())
                .await
                .unwrap();
        }
    });
    rpc(
        &mut writer,
        &mut reader,
        1,
        "initialize",
        json!({
            "protocolVersion":"2024-11-05","capabilities":{},
            "clientInfo":{"name":"native-worker-fixture","version":"1"}
        }),
    )
    .await;
    writer
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();
    let mut id = 2;
    let initial = result(
        rpc(
            &mut writer,
            &mut reader,
            id,
            "tools/call",
            call_params("status"),
        )
        .await,
    );
    assert_eq!(initial["title"], "Synthetic visible track 0");
    for (action, status, track) in [
        ("play", "Playing", 0),
        ("pause", "Paused", 0),
        ("toggle", "Playing", 0),
        ("next", "Playing", 1),
        ("previous", "Playing", 0),
        ("stop", "Stopped", 0),
    ] {
        id += 1;
        assert_eq!(
            result(
                rpc(
                    &mut writer,
                    &mut reader,
                    id,
                    "tools/call",
                    call_params(action)
                )
                .await
            ),
            json!({"ok":true})
        );
        id += 1;
        let live = result(
            rpc(
                &mut writer,
                &mut reader,
                id,
                "tools/call",
                call_params("status"),
            )
            .await,
        );
        assert_eq!(live["status"], status);
        assert_eq!(live["title"], format!("Synthetic visible track {track}"));
    }
    use std::io::Write;
    ui.0.stdin
        .as_mut()
        .unwrap()
        .write_all(b"ui-title:Native UI source\n")
        .unwrap();
    assert_eq!(ready(&mut ui), "updated");
    id += 1;
    assert_eq!(
        result(
            rpc(
                &mut writer,
                &mut reader,
                id,
                "tools/call",
                call_params("status")
            )
            .await
        )["title"],
        "Native UI source"
    );
    crate::approvals::app_policy::revoke(
        uid,
        "cosmic-player",
        crate::caps::Cap::new(Verb::DESKTOP_MEDIA_CONTROL, Scope::name("cosmic-player")),
    )
    .unwrap();
    id += 1;
    assert_eq!(
        rpc(
            &mut writer,
            &mut reader,
            id,
            "tools/call",
            call_params("play")
        )
        .await["result"]["isError"],
        true
    );
    drop(ui);
    id += 1;
    let absent = rpc(
        &mut writer,
        &mut reader,
        id,
        "tools/call",
        call_params("status"),
    )
    .await;
    assert_eq!(absent["result"]["isError"], true, "{absent}");
    assert!(
        absent.to_string().contains("no native Media Player"),
        "{absent}"
    );
    assert!(other.0.try_wait().unwrap().is_none());
    server.await.unwrap();
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    assert_eq!(authority().revoke(session_grant.id), 1);
    assert_eq!(authority().revoke(relay_grant.id), 1);
    drop(prepared.resources);
}
