use super::*;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;

use super::super::app_data::tests::{chown, copy_executable, mount_tmpfs};
use crate::clawd::client_identity::AuthenticatedExtensionHost;
use crate::provenance::trust::TrustRootSpec;

const ACTOR_TEST: &str = "extension_host::spawn::task_data::tests::ordinary_task_app_actor";
const APP: &str = "task-storage";

#[test]
fn task_app_data_bounds_distinct_views_without_replacing_existing_ones() {
    TaskAppData::require_capacity(0).unwrap();
    TaskAppData::require_capacity(63).unwrap();
    assert!(TaskAppData::require_capacity(64).is_err());
    assert!(TaskAppData::require_capacity(65).is_err());
}

const MAIN: &str = r#"
import os, pathlib, sqlite3

def run(command, args):
    root = pathlib.Path(os.environ["COS_DATA_DIR"])
    assert os.environ["COS_APP_ID"] == "task-storage"
    assert not (root.parent / "foreign-app").exists()
    with sqlite3.connect(root / "state.db") as db:
        db.execute("PRAGMA journal_mode=WAL")
        db.execute("CREATE TABLE IF NOT EXISTS counter (value INTEGER)")
        db.execute("INSERT INTO counter SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM counter)")
        db.execute("UPDATE counter SET value=value+1")
        value = db.execute("SELECT value FROM counter").fetchone()[0]
    return {"value": value, "root": str(root)}
"#;

fn trust_roots() -> Vec<TrustRootSpec> {
    vec![TrustRootSpec {
        path: "/run/task-trust/publishers.d".into(),
        tier: crate::provenance::TrustTier::User,
        allowed_uids: vec![0],
        domain: crate::provenance::state::TrustDomain::System,
    }]
}

fn load_trust() {
    let roots = trust_roots();
    let trust = crate::provenance::TrustStore::load_roots(&roots);
    assert!(!trust.is_empty(), "{:?}", trust.diagnostics());
    crate::provenance::set_trust_store_for_roots(trust, roots);
}

#[test]
#[ignore = "private child of task_app_registration_binds_persistent_data_after_authorization"]
fn ordinary_task_app_actor() {
    assert_eq!(std::env::var("COS_TASK_DATA_ACTOR").unwrap(), "1");
    assert_ne!(unsafe { libc::geteuid() }, 0);
    load_trust();
    std::io::stdin().read_exact(&mut [0_u8; 1]).unwrap();
    let apps = PathBuf::from(std::env::var_os("COS_APPS_DIR").unwrap());
    let app = crate::apps::find_verified(&apps, APP).unwrap();
    let launch = crate::bridge::AppLaunch::new(app.require_verified().unwrap().clone()).unwrap();
    let start: i64 = std::env::var("COS_TASK_DATA_EXPECTED")
        .unwrap()
        .parse()
        .unwrap();
    for expected in [start, start + 1] {
        let output = crate::bridge::run_task_app(&launch, "bump", &[], apps.to_str().unwrap())
            .unwrap()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["value"], expected);
        let root = PathBuf::from(value["root"].as_str().unwrap());
        assert!(root.ends_with(format!("app-data/apps/{APP}")));
        assert!(!root.starts_with(std::env::var_os("HOME").unwrap()));
    }
}

fn binding(
    owner: &WorkerIdentity,
    execution: &ExtensionIdentity,
    paths: &HostPaths,
    pid: u32,
) -> ExtensionBinding {
    ExtensionBinding {
        protocol: protocol::PROTOCOL_VERSION,
        purpose: protocol::HostPurpose::Task,
        task_id: format!("storage-{}", uuid::Uuid::new_v4().simple()),
        session_id: Some("task-parent".into()),
        app_id: None,
        owner_uid: owner.uid,
        extension_uid: execution.uid,
        owner_gid: execution.gid,
        capability_generation: "a".repeat(16),
        package: None,
        approved_paths: Vec::new(),
        agent_extensions: Vec::new(),
        controller_uid: owner.uid,
        controller_gid: execution.gid,
        controller_pid: std::process::id(),
        controller_start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        host_pid: pid,
        host_start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
        lease_nonce: "b".repeat(32),
        expires_at_ms: crate::agentd::grant::now_ms() + 120_000,
        control_socket: paths.control_socket.to_string_lossy().into_owned(),
        broker_socket: paths.broker_socket.to_string_lossy().into_owned(),
    }
}

fn task_client(data: &Arc<TaskAppData>) -> ClientIdentity {
    ClientIdentity::from_verified_delegation(
        data.binding.host_pid,
        data.owner.uid,
        data.execution.uid,
        data.execution.gid,
        data.binding.host_start_time_ticks.unwrap(),
        AuthenticatedExtensionHost {
            purpose: data.binding.purpose,
            lease_id: data.binding.task_id.clone(),
            authority_session_id: data.binding.session_id.clone(),
            host_session_id: Some("task-host".into()),
            owner_uid: data.owner.uid,
            extension_uid: data.execution.uid,
            capability_generation: data.binding.capability_generation.clone(),
            host_pid: data.binding.host_pid,
            host_start_time_ticks: data.binding.host_start_time_ticks,
            task_app_data: Some(data.clone()),
        },
    )
}

fn session(id: &str, pid: u32, caps: crate::caps::CapSet) -> crate::proc::SessionInfo {
    crate::proc::SessionInfo {
        session_id: id.into(),
        pid,
        command: vec!["claw-extension-host".into(), "task-data-fixture".into()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some(protocol::EXTENSION_HOST_GROUP.into()),
        parent: None,
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: Some(crate::caps::Role::Worker.credential_tier()),
        scope: None,
        priority: None,
        caps: Some(caps),
        transient_caps: None,
        role: Some(crate::caps::Role::Worker.name().into()),
        app_id: None,
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
        client: crate::session::SessionClient::new(
            crate::session::SessionSource::BrokerTask,
            false,
            true,
        ),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Root in private mount/network namespaces and COS_APP_DATA_TEST_COS"]
async fn task_app_registration_binds_persistent_data_after_authorization() {
    use crate::caps::{Cap, CapSet, Scope, Verb};
    use crate::clawd::transport::{frame::PeerStream, peer, ReadOutcome};
    use crate::clawd::wire::Request;

    let _lock = crate::test_env::lock_env();
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert_ne!(
        std::fs::read_link("/proc/self/ns/mnt").unwrap(),
        std::fs::read_link("/proc/1/ns/mnt").unwrap()
    );
    let namespace = open_namespace(c"/proc/self/ns/mnt").unwrap();
    let failed = in_namespace(&namespace, || {
        Err(std::io::Error::from_raw_os_error(libc::EIO))
    });
    assert!(failed.unwrap_err().contains("mount helper"));
    let started = Instant::now();
    let timed_out = in_namespace(&namespace, || {
        unsafe { libc::poll(std::ptr::null_mut(), 0, 30_000) };
        Ok(())
    });
    assert!(timed_out.unwrap_err().contains("timed out"));
    assert!(started.elapsed() >= MOUNT_HELPER_TIMEOUT);
    assert!(started.elapsed() < Duration::from_secs(8));
    for task in std::fs::read_dir("/proc/self/task").unwrap() {
        let children = task.unwrap().path().join("children");
        match std::fs::read_to_string(children) {
            Ok(children) => assert!(children.trim().is_empty(), "mount helper was not reaped"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("inspect namespace-helper cleanup: {error}"),
        }
    }
    let owner = crate::agentd::spawn::resolve_identity(
        std::fs::metadata(env!("CARGO_MANIFEST_DIR")).unwrap().uid(),
    )
    .unwrap();
    let backing = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    chown(backing.path(), owner.uid, owner.gid);
    mount_tmpfs(c"/run");
    std::fs::create_dir("/run/lock").unwrap();
    std::fs::set_permissions("/run/lock", std::fs::Permissions::from_mode(0o1777)).unwrap();
    mount_tmpfs(c"/usr/local/bin");
    copy_executable(
        Path::new(&std::env::var("COS_APP_DATA_TEST_COS").unwrap()),
        Path::new("/usr/local/bin/cos"),
    );
    copy_executable(
        &std::env::current_exe().unwrap(),
        Path::new("/run/task-data-actor"),
    );
    for path in [
        "/run/task-runtime",
        "/run/task-state",
        "/run/task-log",
        "/run/task-trust/publishers.d",
        "/run/task-apps",
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    let _runtime = crate::test_env::TestEnvVarGuard::set("COS_RUNTIME_DIR", "/run/task-runtime");
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", "/run/task-state");
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", "/run/task-state");
    let _logs = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", "/run/task-log");
    let _user_data = crate::test_env::TestEnvVarGuard::set("COS_USER_DATA_DIR", backing.path());
    let _apps = crate::test_env::TestEnvVarGuard::set("COS_APPS_DIR", "/run/task-apps");
    let key =
        crate::provenance::sign::SigningKeyFile::generate(Some("private Task data fixture".into()))
            .unwrap();
    let key_file = Path::new("/run/task-trust/publishers.d/key.json");
    std::fs::write(
        key_file,
        serde_json::to_vec(&key.trust_entry(&[crate::provenance::PackageKind::App])).unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(key_file, std::fs::Permissions::from_mode(0o444)).unwrap();
    crate::test_env::record_trust_state(&trust_roots());
    load_trust();
    let app_dir = Path::new("/run/task-apps").join(APP);
    std::fs::create_dir(&app_dir).unwrap();
    std::fs::write(app_dir.join("main.py"), MAIN).unwrap();
    let manifest = serde_json::json!({
        "schema_version": 2, "id": APP, "version": "0.1.0", "runtime": "python",
        "name": {"en": "Task storage fixture"},
        "operations": {
            "bump": {"label": {"en": "Bump"}, "args": [], "needs": []},
            "denied": {"label": {"en": "Denied"}, "args": [], "needs": [{
                "verb": "sys.package",
                "scope": {"kind": "fixed", "scope": {"kind": "name", "value": "fixture"}},
                "why": {"en": "Fixture denial"}
            }]}
        },
    });
    std::fs::write(app_dir.join("app.json"), manifest.to_string()).unwrap();
    crate::provenance::sign::sign_directory(
        &app_dir,
        &crate::provenance::sign::SignRequest {
            kind: crate::provenance::PackageKind::App,
            id: APP.into(),
            version: "0.1.0".into(),
            manifest_schema: "2".into(),
            manifest_path: "app.json".into(),
            entrypoints: vec!["main.py".into()],
            resources: Vec::new(),
        },
        &key,
    )
    .unwrap();
    let app = crate::apps::find_verified(Path::new("/run/task-apps"), APP).unwrap();
    let package = app.require_verified().unwrap();
    let review = crate::approvals::system_review::submit(
        owner.uid,
        crate::approvals::system_review::ReviewKind::AppActivation,
        package,
        "private fixture".into(),
    )
    .unwrap();
    crate::approvals::system_review::decide(owner.uid, &review.id, true, Some(package)).unwrap();
    crate::approvals::system_review::consume(owner.uid, &review.id, package).unwrap();
    let caps = CapSet::from_caps([Cap::new(Verb::AGENT_INVOKE, Scope::name(APP))]);
    for (execution_uid, expected) in [(61010, 1), (61011, 3)] {
        let execution = ExtensionIdentity {
            uid: execution_uid,
            gid: 65534,
            username: "task-data-fixture".into(),
        };
        let paths = HostPaths::create(&owner).unwrap();
        let listener =
            crate::extension_host::broker::bind_listener(&paths, execution.uid, execution.gid)
                .unwrap();
        let mut command = std::process::Command::new("/run/task-data-actor");
        command
            .args([
                "--exact",
                ACTOR_TEST,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", &paths.control_dir)
            .env("COS_DATA_DIR", paths.control_dir.join("data"))
            .env("COS_LOG_DIR", paths.control_dir.join("log"))
            .env("COS_CACHE_DIR", paths.control_dir.join("cache"))
            .env("COS_APPS_DIR", "/run/task-apps")
            .env("COS_PROC_DATA_DIR", format!("/run/cos/caps/{}", owner.uid))
            .env("COS_SESSION", "task-host")
            .env("COS_EXTENSION_BROKER_SOCKET", &paths.broker_socket)
            .env("XDG_RUNTIME_DIR", "/tmp")
            .env("COS_TASK_DATA_ACTOR", "1")
            .env("COS_TASK_DATA_EXPECTED", expected.to_string())
            .current_dir(&paths.control_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let task_path = CString::new(paths.dir.as_os_str().as_bytes()).unwrap();
        unsafe {
            command.pre_exec(move || {
                setup_private_mount_namespace(&task_path)?;
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(65534) != 0
                    || libc::setuid(execution_uid) != 0
                    || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                crate::agentd::spawn::mark_inherited_descriptors_cloexec(3);
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        drop(command);
        let mut namespace = PrivateMountNamespace::capture(child.id(), &paths.dir, None).unwrap();
        let data = Arc::new(TaskAppData::new(
            owner.clone(),
            execution.clone(),
            binding(&owner, &execution, &paths, child.id()),
            paths.clone(),
            namespace.fd.clone(),
        ));
        namespace.task_app_data = Some(data.clone());
        let client = task_client(&data);
        let diagnostic = format!("{client:?}");
        assert!(!diagnostic.contains(&data.binding.lease_nonce));
        assert!(!diagnostic.contains(paths.dir.to_str().unwrap()));
        assert!(data.views.lock().unwrap().is_empty());
        for mutate in [
            |client: &mut ClientIdentity| client.uid = Some(0),
            |client: &mut ClientIdentity| client.pid = Some(std::process::id()),
            |client: &mut ClientIdentity| {
                client.extension_host.as_mut().unwrap().lease_id = "different-task".into()
            },
        ] {
            let mut forged = client.clone();
            mutate(&mut forged);
            assert!(data.bind_app(&forged, APP).is_err());
            assert!(data.views.lock().unwrap().is_empty());
        }
        crate::proc::register_session_for_owner(
            session("task-host", child.id(), caps.clone()),
            owner.uid,
        )
        .unwrap();
        crate::storage::install_routed_extension_reader(owner.uid, execution.uid).unwrap();
        for operation in ["absent", "denied"] {
            let request = serde_json::json!({
                "app_id": APP, "kind": "operation", "operation": operation, "args": [],
                "package": PackageRef::of(package),
            });
            assert!(crate::clawd::app_sessions::register(request, &client)
                .await
                .is_err());
            assert!(
                data.views.lock().unwrap().is_empty(),
                "a rejected operation mounted data"
            );
        }
        let invalid = serde_json::json!({
            "app_id": APP, "kind": "operation", "operation": "bump", "args": [],
            "package": PackageRef::of(package), "task_app_data_dir": "/caller/path",
        });
        assert!((crate::clawd::routes::Command::AppSessionRegister
            .route()
            .decode)(invalid)
        .is_err());
        let state = crate::clawd::state::DaemonState::try_new().unwrap();
        let admission = crate::clawd::transport::limits::Admission::new(
            crate::clawd::transport::limits::Limits::default(),
        );
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let broker = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! { _ = &mut stopped => break, accepted = listener.accept() => accepted };
                let mut stream = PeerStream::new(accepted.unwrap().0).unwrap();
                let ReadOutcome::Frame(frame) = stream
                    .read_request(crate::clawd::wire::MAX_REQUEST_BYTES)
                    .await
                    .unwrap()
                else {
                    continue;
                };
                let process = peer::verify(frame.credentials).unwrap();
                assert_eq!(process.pid, client.pid.unwrap());
                assert_eq!(process.uid, client.execution_uid.unwrap());
                assert_eq!(process.start_time_ticks, client.start_time_ticks.unwrap());
                let request: Request = serde_json::from_slice(&frame.body).unwrap();
                let response = crate::clawd::server::dispatch_verified_request(
                    request, &client, &state, &admission,
                )
                .await;
                stream
                    .write_response(&crate::clawd::protocol::encode_response(&response).unwrap())
                    .await
                    .unwrap();
            }
        });
        child.stdin.take().unwrap().write_all(b"1").unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                break child.wait().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        data.close();
        assert!(data.bind_app(&task_client(&data), APP).is_err());
        let stopped = stop.send(());
        let served = broker.await;
        let count = data.views.lock().unwrap().len();
        namespace.cleanup().unwrap();
        paths.cleanup().unwrap();
        crate::proc::deregister_session_for_owner("task-host", owner.uid);
        crate::storage::remove_routed_extension_reader(owner.uid, execution.uid).unwrap();
        assert!(
            stopped.is_ok(),
            "private broker stopped before the fixture completed"
        );
        served.unwrap();
        assert!(status.success(), "ordinary App task failed: {status}");
        assert_eq!(count, 1, "same-App registrations share one held view");
        assert!(data.views.lock().unwrap().is_empty());
        let db_path = backing.path().join("apps").join(APP).join("state.db");
        let metadata = std::fs::metadata(&db_path).unwrap();
        assert_eq!((metadata.uid(), metadata.gid()), (owner.uid, owner.gid));
        let value: i64 = {
            let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(owner.uid).unwrap();
            rusqlite::Connection::open(db_path)
                .unwrap()
                .query_row("SELECT value FROM counter", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(value, expected + 1);
    }
}
