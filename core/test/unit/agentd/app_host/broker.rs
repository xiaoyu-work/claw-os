use super::*;
use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::clawd::protocol::RequestId;
use crate::session::journal::harness::Harness;
use crate::test_env::TestEnvVarGuard;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    _env: Vec<TestEnvVarGuard>,
    harness: Harness,
    process: Child,
    child: ProcessIdentity,
    host: Arc<AppHost>,
    original: AppInvocation,
}

impl Fixture {
    fn new() -> Self {
        let harness = Harness::new();
        let _lease = harness.lease();
        let root = harness.data_dir();
        let env = vec![
            TestEnvVarGuard::set("COS_APPS_DIR", root.join("apps")),
            TestEnvVarGuard::set("COS_CAPS_DATA_DIR", root.join("caps")),
            TestEnvVarGuard::set(
                "COS_PROC_DATA_DIR",
                if unsafe { libc::geteuid() } == 0 {
                    std::path::PathBuf::from("/run/cos/caps/65534")
                } else {
                    root.join("registry")
                },
            ),
            TestEnvVarGuard::set("COS_PROVENANCE_RUNTIME_DIR", root.join("runtime")),
        ];
        let app = root.join("apps").join("demo");
        std::fs::create_dir_all(&app).unwrap();
        let work = root.join("workspace");
        std::fs::create_dir(&work).unwrap();
        let path = work.join("input.txt");
        let second = work.join("second.txt");
        std::fs::write(&path, "input").unwrap();
        std::fs::write(&second, "second").unwrap();
        std::fs::write(app.join("app.json"), json!({
            "id":"demo","version":"1","name":"Demo",
            "operations":{"read":{
                "label":"Read",
                "args":[{"name":"path","kind":"path","required":true}],
                "needs":[{"verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},"why":"Read"}]
            }},
            "session":{"entry":"server.py","tools":[{
                "name":"demo.read","summary":"Read in a session",
                "args":[{"name":"path","kind":"path","required":true}],
                "needs":[{"verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},"why":"Read"}]
            }]}
        }).to_string()).unwrap();
        std::fs::write(
            app.join("main.py"),
            "raise AssertionError('broker does not execute Apps')\n",
        )
        .unwrap();
        std::fs::write(
            app.join("server.py"),
            "raise AssertionError('root does not run sessions')\n",
        )
        .unwrap();
        let launch = crate::test_env::app_launch(&app, "demo");
        let original = AppInvocation {
            app_id: "demo".into(),
            operation: "read".into(),
            args: vec!["input.txt".into()],
            package_digest: launch.package().content_digest().to_string(),
        };
        let root_user = unsafe { libc::geteuid() } == 0;
        let uid = if root_user {
            65534
        } else {
            unsafe { libc::geteuid() }
        };
        let gid = if root_user {
            65534
        } else {
            unsafe { libc::getegid() }
        };
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 120 & echo $!; wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0
                    || (root_user
                        && (libc::setgroups(0, std::ptr::null()) != 0
                            || libc::setresgid(gid, gid, gid) != 0
                            || libc::setresuid(uid, uid, uid) != 0))
                    || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut process = command.spawn().unwrap();
        let mut line = String::new();
        BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let child_pid: u32 = line.trim().parse().unwrap();
        let child = ProcessIdentity::of_process(uid, child_pid).unwrap();
        let client = ClientIdentity {
            pid: Some(process.id()),
            uid: Some(uid),
            gid: Some(gid),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(process.id()),
        };
        let caps = CapSet::from_caps([
            Cap::new(Verb::AGENT_INVOKE, Scope::name("demo")),
            Cap::new(Verb::FS_READ, Scope::path(path.to_string_lossy())),
            Cap::new(Verb::FS_READ, Scope::path(second.to_string_lossy())),
        ]);
        let parent: SessionInfo = serde_json::from_value(json!({
            "session_id":"host-parent","pid":std::process::id(),"command":["task"],
            "started_at":"now","stdout_path":"","stderr_path":"",
            "group":"agent","caps":caps,"workdir":work
        }))
        .unwrap();
        let host = AppHost::new(
            "task-owned".into(),
            client,
            Some(parent),
            DaemonState::new().unwrap(),
            Admission::new(crate::clawd::transport::limits::Limits::default()),
            Instant::now() + Duration::from_secs(60),
        );
        Self {
            _env: env,
            harness,
            process,
            child,
            host,
            original,
        }
    }

    async fn call(&self, call: AppHostCall) -> Response {
        self.host
            .handle(AppHostRequest {
                task_id: "task-owned".into(),
                correlation_id: 1,
                request_id: RequestId::generate(),
                call,
            })
            .await
    }

    async fn prepare(&self) -> PreparedInvocation {
        let response = self.call(AppHostCall::Begin(self.original.clone())).await;
        assert!(response.ok, "{response:?}");
        serde_json::from_value(response.result.unwrap()).unwrap()
    }

    async fn register(&self, prepared: &PreparedInvocation, args: &[String]) -> Response {
        let call = serde_json::from_value(json!({
            "method":"register","params":{
                "invocation_id":prepared.id,
                "request":{"app_id":"demo","kind":"operation","operation":"read","args":args}
            }
        }))
        .unwrap();
        self.call(call).await
    }

    async fn bind(&self, registration: &Value) -> Response {
        self.call(
            serde_json::from_value(json!({
                "method":"bind","params":{
                    "session_id":registration["session_id"],"handle":registration["handle"],
                    "pid":self.child.pid
                }
            }))
            .unwrap(),
        )
        .await
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        crate::provenance::runtime::terminate_process_identity(
            &self.child,
            Duration::from_millis(20),
        );
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

fn require_private_root() {
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "this test requires root in a private mount namespace"
    );
    assert_ne!(
        std::fs::read_link("/proc/self/ns/mnt").unwrap(),
        std::fs::read_link("/proc/1/ns/mnt").unwrap(),
        "do not run this fixture in the system mount namespace"
    );
    assert!(
        std::path::Path::new("/run/.cos-app-host-test").is_file(),
        "mount an isolated tmpfs on /run and create its test marker first"
    );
}

mod stateful {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/app_host/broker_stateful.rs"
    ));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires root with an isolated /run tmpfs; see App host MODULE.md"]
async fn retained_original_rejects_changed_registration_arguments() {
    require_private_root();
    let fixture = Fixture::new();
    let prepared = fixture.prepare().await;
    assert!(std::path::Path::new(&prepared.args[0]).is_absolute());
    let changed = vec![fixture
        .harness
        .data_dir()
        .join("other.txt")
        .to_string_lossy()
        .into_owned()];
    let denied = fixture.register(&prepared, &changed).await;
    assert!(
        !denied.ok,
        "changed registration must not compare against itself"
    );
    assert!(fixture.host.sessions.lock().await.active.is_empty());
    let registered = fixture.register(&prepared, &prepared.args).await;
    assert!(registered.ok, "{registered:?}");
    let data = registered.result.unwrap();
    let id = data["session_id"].as_str().unwrap();
    verify_instance(
        fixture.child.uid,
        id,
        &fixture.host.sessions.lock().await.active[id],
    )
    .unwrap();
    fixture.host.close().unwrap().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires root with an isolated /run tmpfs; see App host MODULE.md"]
async fn bind_verifies_persisted_identity_and_close_stops_child_before_forgetting_it() {
    require_private_root();
    let fixture = Fixture::new();
    let prepared = fixture.prepare().await;
    let registered = fixture.register(&prepared, &prepared.args).await;
    assert!(registered.ok, "{registered:?}");
    let data = registered.result.unwrap();
    let bound = fixture.bind(&data).await;
    assert!(bound.ok, "{bound:?}");
    let id = data["session_id"].as_str().unwrap();
    let instance = crate::provenance::runtime::instance_for(fixture.child.uid, id)
        .unwrap()
        .unwrap();
    assert_eq!(instance.class, InstanceClass::App);
    assert_eq!(instance.process.as_ref(), Some(&fixture.child));
    assert!(fixture.child.still_matches());
    let cleanup = fixture.host.close().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if !fixture.child.still_matches()
                && crate::provenance::runtime::instance_for(fixture.child.uid, id)
                    .unwrap()
                    .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    cleanup.await.unwrap();
    assert!(fixture.host.sessions.lock().await.active.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires root with an isolated /run tmpfs; see App host MODULE.md"]
async fn checks_refuse_runtime_binding_drift_without_signalling_the_substitute() {
    require_private_root();
    let fixture = Fixture::new();
    let prepared = fixture.prepare().await;
    let registered = fixture.register(&prepared, &prepared.args).await;
    assert!(registered.ok, "{registered:?}");
    let data = registered.result.unwrap();
    assert!(fixture.bind(&data).await.ok);
    let mut other = ChildGuard(Command::new("/bin/sleep").arg("120").spawn().unwrap());
    let id = data["session_id"].as_str().unwrap();
    crate::provenance::runtime::bind_process(fixture.child.uid, id, other.0.id());
    let check = fixture.call(serde_json::from_value(json!({
        "method":"check","params":{"session_id":id,"package_digest":fixture.original.package_digest}
    })).unwrap()).await;
    assert!(!check.ok);
    let cleanup = fixture.host.close().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.child.still_matches() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    cleanup.await.unwrap();
    assert!(
        other.0.try_wait().unwrap().is_none(),
        "must not signal the mismatched runtime identity"
    );
    assert!(
        crate::provenance::runtime::instance_for(fixture.child.uid, id)
            .unwrap()
            .is_some()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn host_control_limits_and_lifetime_do_not_cancel_in_flight_work() {
    let fixture = Fixture::new();
    let mut permits = Vec::new();
    for _ in 0..MAX_ACTIVE_CALLS {
        permits.push(fixture.host.reserve().unwrap());
    }
    assert!(fixture.host.reserve().is_err());
    permits.pop();
    assert!(fixture.host.reserve().is_ok());
    let running = fixture.host.sessions.lock().await;
    let cleanup = fixture.host.close().unwrap();
    assert!(fixture.host.closed.load(Ordering::SeqCst));
    assert!(fixture.host.sessions.try_lock().is_err());
    drop(running);
    cleanup.await.unwrap();
    let refused = fixture
        .call(AppHostCall::Begin(fixture.original.clone()))
        .await;
    assert!(!refused.ok);
}
