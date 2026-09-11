use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::path::Path;

use crate::clawd::protocol::{encode_response, Response};
use crate::clawd::transport::{frame::PeerStream, peer, ReadOutcome};
use crate::clawd::wire::{InboundRequest, MAX_REQUEST_BYTES, PROTOCOL_VERSION};
use crate::test_env::TestEnvVarGuard;
use crate::worker::{BrokerAuthority, RelayHandle, WorkerLaunch};

fn copy_python_package(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "py")
        {
            std::fs::copy(entry.path(), destination.join(entry.file_name())).unwrap();
        }
    }
}

async fn serve_replacement(listener: tokio::net::UnixListener) {
    let state = crate::clawd::state::DaemonState::new().unwrap();
    loop {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = PeerStream::new(stream).unwrap();
        let ReadOutcome::Frame(frame) = stream.read_request(MAX_REQUEST_BYTES).await.unwrap()
        else {
            panic!("expected a framed worker relay request");
        };
        peer::verify(frame.credentials).unwrap();
        let client = ClientIdentity {
            uid: Some(frame.credentials.uid),
            gid: Some(frame.credentials.gid),
            pid: Some(frame.credentials.pid),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(frame.credentials.pid),
        };
        let envelope: InboundRequest = serde_json::from_slice(&frame.body).unwrap();
        assert_eq!(envelope.v, PROTOCOL_VERSION);
        assert_eq!(envelope.command.as_str(), Command::AppSessionRelay.as_str());
        let route = Command::AppSessionRelay.route();
        route.authorize(&client).unwrap();
        let params = (route.decode)(envelope.params).unwrap();
        let decision = authority::authorize(route.name, &route.authority, &params, &client)
            .await
            .unwrap()
            .unwrap();
        let bracket = crate::clawd::journal::begin(route, &envelope.id, Some(&decision), &client)
            .unwrap()
            .unwrap();
        let result = (route.handler)(crate::clawd::routes::RouteCall {
            state: &state,
            client: &client,
            params,
            authority: Some(&decision),
        })
        .await;
        assert!(authority::obligation_met(Some(&decision)));
        let response = match result {
            Ok(value) => Response::ok(envelope.id.clone(), value),
            Err(error) => route.errors.response(envelope.id.clone(), error),
        };
        let response =
            crate::clawd::journal::finish(bracket, &envelope.id, &response).unwrap_or(response);
        stream
            .write_response(&encode_response(&response).unwrap())
            .await
            .unwrap();
    }
}

struct AppProcess {
    apps: PathBuf,
    sdk: PathBuf,
    data: PathBuf,
    session: String,
    relay: RelayHandle,
}

impl AppProcess {
    async fn run(&self, operation: &str, args: Value, caps: CapSet) -> Value {
        let script = r#"
import json, os, sys
sys.path.insert(0, os.path.join(os.environ["COS_APPS_DIR"], "fs"))
import main
result = main.run(sys.argv[1], json.loads(sys.argv[2]))
print(json.dumps(result))
"#;
        let policy =
            crate::worker::derive::app_operation(crate::worker::derive::AppOperationInput {
                package_identity: None,
                pinned_entries: Vec::new(),
                developer: false,
                app_id: "fs",
                app_dir: &self.apps.join("fs"),
                operation,
                program: PathBuf::from("/usr/bin/python3"),
                argv: vec![
                    "-c".to_string(),
                    script.to_string(),
                    operation.to_string(),
                    args.to_string(),
                ],
                caps: &caps,
                session_id: &self.session,
                data_dir: self.data.to_str().unwrap(),
                apps_dir: self.apps.to_str().unwrap(),
                extra_env: BTreeMap::from([(
                    "PYTHONPATH".to_string(),
                    format!("{}:{}", self.sdk.display(), self.apps.display()),
                )]),
                stdio: crate::worker::StdioPlan::Captured,
                desktop: false,
            })
            .unwrap();
        for cap in caps.iter().filter(|cap| cap.verb == Verb::FS_WRITE) {
            if let Scope::Path(path) = &cap.scope {
                if Path::new(path).is_file() {
                    assert!(
                        !policy.mounts.iter().any(|mount| {
                            Some(mount.source.as_path()) == Path::new(path).parent()
                        }),
                        "existing-file replacement must not mount its parent"
                    );
                }
            }
        }
        let limits = policy.limits;
        let launch = WorkerLaunch::new(policy).with_authority(BrokerAuthority::new(
            &self.session,
            Some("fs".to_string()),
            caps,
            self.relay.clone(),
        ));
        let result = tokio::task::spawn_blocking(move || {
            let prepared = crate::worker::prepare(&launch).unwrap();
            crate::worker::run_captured(prepared, None, limits, |_| Ok(())).unwrap()
        })
        .await
        .unwrap();
        assert!(
            result.status.success(),
            "App process failed: {}{}",
            result.stdout_string(),
            result.stderr_string()
        );
        serde_json::from_str(&result.stdout_string()).unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires COS_FILE_PLAN_COS_BIN pointing to a freshly built cos binary and a working sandbox"]
async fn file_plan_real_app_worker_cli_and_broker_round_trip() {
    let binary = PathBuf::from(
        std::env::var_os("COS_FILE_PLAN_COS_BIN").expect("set COS_FILE_PLAN_COS_BIN"),
    )
    .canonicalize()
    .unwrap();
    assert!(crate::worker::availability().is_available());
    let fixture = Fixture::new(Some(b"before\n"));
    let root = fixture.harness.data_dir();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let sdk = root.join("python");
    for (source, package) in [
        ("cos-runtime", "cos_runtime"),
        ("claw-os-sdk", "claw_os_sdk"),
    ] {
        copy_python_package(
            &repo.join(source).join("python").join("src").join(package),
            &sdk.join(package),
        );
    }
    let runtime = root.join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    let _binary = TestEnvVarGuard::set("COS_BIN", binary);
    let _sdk = TestEnvVarGuard::set("COS_SDK_PYTHON_DIR", &sdk);
    let _runtime = TestEnvVarGuard::set("COS_RUNTIME_DIR", &runtime);
    let _instances = TestEnvVarGuard::set("COS_PROVENANCE_RUNTIME_DIR", root.join("instances"));
    let _proc = TestEnvVarGuard::set("COS_PROC_DATA_DIR", root.join("proc"));
    crate::provenance::runtime::register_operator_mcp(
        fixture.harness.owner_uid(),
        &fixture.session,
    );
    let read = Cap::new(Verb::FS_READ, Scope::path(fixture.path.to_string_lossy()));
    let write = Cap::new(Verb::FS_WRITE, Scope::path(fixture.path.to_string_lossy()));
    let store_read = Cap::new(Verb::DATA_DB_READ, Scope::name("fs-change-plans"));
    let store_write = Cap::new(Verb::DATA_DB_WRITE, Scope::name("fs-change-plans"));
    let all = vec![read.clone(), write, store_read.clone(), store_write.clone()];
    let _decision = fixture.decision(all.clone());
    let (relay, _) = authority::authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(fixture.harness.owner_uid(), std::process::id())
                .unwrap(),
            binding: Binding::Process,
            subject: Subject::session(&fixture.session),
            audience: AudienceSet::one(Audience::AppRelay),
            caps: CapSet::from_caps([]),
            lifetime: std::time::Duration::from_secs(300),
            uses: Uses::Unbounded,
            index_session: false,
        })
        .unwrap();
    let relay_slot = crate::worker::relay_slot();
    crate::worker::install_relay(&relay_slot, Some(relay.into_wire()));
    let listener = tokio::net::UnixListener::bind(runtime.join("clawd.sock")).unwrap();
    peer::enable_credential_passing(listener.as_raw_fd()).unwrap();
    let server = tokio::spawn(serve_replacement(listener));
    let app = AppProcess {
        apps: repo.join("apps").canonicalize().unwrap(),
        sdk,
        data: root,
        session: fixture.session.clone(),
        relay: relay_slot,
    };

    let draft = app
        .run(
            "plan_write",
            json!([fixture.path, "--content", "after\n"]),
            CapSet::from_caps([read.clone(), store_write.clone()]),
        )
        .await;
    assert_eq!(draft["state"], "draft", "{draft}");
    claw_os_sdk::generated::validate_file_change_plan(&draft).unwrap();
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"before\n");
    assert!(draft["diff"]
        .as_str()
        .unwrap()
        .contains("-before\n+after\n"));

    let arguments = json!([
        fixture.path,
        "--plan",
        draft["plan_id"],
        "--review",
        draft["review"],
        "--confirm=true"
    ]);
    let applied = app
        .run(
            "plan_apply",
            arguments.clone(),
            CapSet::from_caps(all.clone()),
        )
        .await;
    assert_eq!(applied["state"], "applied", "{applied}");
    assert_eq!(applied["changed"], true);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), b"after\n");
    assert_eq!(
        app.run(
            "plan_show",
            json!([fixture.path, "--plan", draft["plan_id"]]),
            CapSet::from_caps([read.clone(), store_read]),
        )
        .await["state"],
        "applied"
    );
    assert_eq!(
        app.run("plan_apply", arguments, CapSet::from_caps(all.clone()))
            .await["code"],
        "plan_consumed"
    );
    std::fs::remove_file(&fixture.path).unwrap();
    let content = "x".repeat(MAX_FILE_BYTES);
    let creation = app
        .run(
            "plan_write",
            json!([fixture.path, "--content", content]),
            CapSet::from_caps([read, store_write]),
        )
        .await;
    assert_eq!(creation["state"], "draft", "{creation}");
    assert_eq!(creation["before_exists"], false);
    assert!(!fixture.path.exists());
    claw_os_sdk::generated::validate_file_change_plan(&creation).unwrap();
    let created = app
        .run(
            "plan_apply",
            json!([
                fixture.path,
                "--plan",
                creation["plan_id"],
                "--review",
                creation["review"],
                "--confirm=true"
            ]),
            CapSet::from_caps(all),
        )
        .await;
    assert_eq!(created["state"], "applied", "{created}");
    assert_eq!(created["after_bytes"], MAX_FILE_BYTES);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), content.as_bytes());
    assert_eq!(
        std::fs::metadata(&fixture.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let partition = Partition::Session(fixture.session.parse().unwrap());
    let view = journal::projection::build(&partition, fixture.harness.owner_uid()).unwrap();
    assert_eq!(view.mutations.len(), 4);
    assert!(view
        .mutations
        .iter()
        .all(|mutation| mutation.status == "committed"));
    assert!(
        journal::unresolved_mutations(&partition, fixture.harness.owner_uid())
            .unwrap()
            .is_empty()
    );
    fixture.assert_no_stages();
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
