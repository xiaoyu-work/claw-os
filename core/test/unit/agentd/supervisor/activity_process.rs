// Opt-in replacement for the removed app_host process fixture. It uses main's
// real supervisor, isolated task Host and owner/App service manager. The fixture
// must already be installed/verified/reviewed; this test installs no trust roots.
use super::*;
use crate::activities::{ActivityService, ActivityState, ReceiptSource};
use crate::caps::{CapSet, Role};
use crate::test_env::TestEnvVarGuard;
use serde::Deserialize;
use serde_json::{json, Value};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;

const CHILD: &str = "agentd::worker::tests::activity_process::activity_extension_worker_child";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    owner_uid: u32,
    owner_home: PathBuf,
    data_root: PathBuf,
    caps_root: PathBuf,
    apps_root: PathBuf,
    app_id: String,
    caps: CapSet,
    extension_host_binary: PathBuf,
    cos_binary: PathBuf,
    app_runner_binary: PathBuf,
    operation: String,
    operation_args: Vec<String>,
    tool: String,
    first_arguments: Value,
    second_arguments: Value,
    counter_file: PathBuf,
}

fn require_private_root() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "Root is required");
    assert_ne!(
        std::fs::read_link("/proc/self/ns/mnt").unwrap(),
        std::fs::read_link("/proc/1/ns/mnt").unwrap(),
        "use a private mount namespace, never the host /run",
    );
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
    assert!(
        mounts.lines().any(|line| {
            line.split_whitespace().nth(4) == Some("/run") && line.contains(" - tmpfs ")
        }),
        "mount an isolated tmpfs at /run before executing this fixture"
    );
}

fn root_file(path: &std::path::Path) {
    let meta = std::fs::symlink_metadata(path).unwrap();
    assert!(meta.is_file() && !meta.file_type().is_symlink());
    assert_eq!(meta.uid(), 0);
    assert_eq!(meta.mode() & 0o022, 0);
}

fn quote(path: &std::path::Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}

fn counter(path: &std::path::Path) -> u64 {
    match std::fs::read_to_string(path) {
        Ok(value) => value.trim().parse().expect("numeric real App counter"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("read real App counter: {error}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Root/private-/run, COS_ACTIVITY_HOST_FIXTURE, installed verified/reviewed App, reserved UID pool, cgroup.kill, idmapped mounts, bubblewrap and fresh binaries"]
async fn activity_task_and_service_hosts_preserve_real_effects_receipts_and_limits() {
    let _lock = crate::test_env::lock_env();
    require_private_root();
    struct BrokerFlag(bool);
    impl Drop for BrokerFlag {
        fn drop(&mut self) {
            if !self.0 {
                crate::agentd::guard::clear_broker_process_for_test();
            }
        }
    }
    let _broker_flag = BrokerFlag(crate::agentd::guard::is_broker_process());
    crate::agentd::guard::mark_broker_process();
    let fixture_path = PathBuf::from(
        std::env::var_os("COS_ACTIVITY_HOST_FIXTURE")
            .expect("prepared root-owned fixture descriptor"),
    );
    root_file(&fixture_path);
    let bytes = std::fs::read(&fixture_path).unwrap();
    assert!(bytes.len() <= 64 * 1024);
    let fixture: Fixture = serde_json::from_slice(&bytes).unwrap();
    assert_ne!(fixture.owner_uid, 0);
    for path in [
        &fixture.owner_home,
        &fixture.data_root,
        &fixture.caps_root,
        &fixture.counter_file,
    ] {
        assert!(
            path.is_absolute() && path.starts_with("/run"),
            "fixture data must stay in the private /run namespace: {}",
            path.display()
        );
    }
    for path in [&fixture.data_root, &fixture.caps_root] {
        let meta = std::fs::symlink_metadata(path).unwrap();
        assert!(meta.is_dir() && !meta.file_type().is_symlink());
        assert_eq!(meta.uid(), 0);
        assert_eq!(meta.mode() & 0o022, 0);
    }
    for binary in [
        &fixture.extension_host_binary,
        &fixture.cos_binary,
        &fixture.app_runner_binary,
    ] {
        root_file(binary);
    }
    for case in [
        "one_shot",
        "stateful",
        "deny",
        "limit_disable",
        "limit_revision",
        "limit_expiry",
        "policy_disable",
        "policy_revision",
        "policy_introduce",
    ] {
        let runtime = tempfile::Builder::new()
            .prefix("activity-host-main-")
            .tempdir_in("/run")
            .unwrap();
        std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let _data = TestEnvVarGuard::set("COS_DATA_DIR", &fixture.data_root);
        let _caps = TestEnvVarGuard::set("COS_CAPS_DATA_DIR", &fixture.caps_root);
        let _runtime = TestEnvVarGuard::set("COS_RUNTIME_DIR", runtime.path().join("runtime"));
        let _apps = TestEnvVarGuard::set("COS_APPS_DIR", &fixture.apps_root);
        let _extension =
            TestEnvVarGuard::set("COS_EXTENSION_HOST_BIN", &fixture.extension_host_binary);
        let _cos = TestEnvVarGuard::set("COS_BIN", &fixture.cos_binary);
        let _runner = TestEnvVarGuard::set("COS_APP_RUNNER_BIN", &fixture.app_runner_binary);
        let verified =
            crate::apps::find_verified_fresh(&fixture.apps_root, &fixture.app_id).unwrap();
        let package =
            crate::provenance::runtime::PackageRef::of(verified.require_verified().unwrap());
        let identity = spawn::resolve_identity(fixture.owner_uid).unwrap();
        let primary = runtime.path().join("clawd.sock");
        let listener = std::os::unix::net::UnixListener::bind(&primary).unwrap();
        let raw = std::ffi::CString::new(primary.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::chown(raw.as_ptr(), 0, identity.gid) }, 0);
        std::fs::set_permissions(&primary, std::fs::Permissions::from_mode(0o660)).unwrap();
        let state = DaemonState::try_new().unwrap();
        let broker =
            BrokerContext::new(state.clone(), Admission::new(Limits::default()), primary).unwrap();
        // No test containment or UID fallback: unavailable installed isolation
        // must fail this opt-in run instead of producing a success-shaped skip.
        broker.extension_containment().unwrap();
        broker.extension_identity_pool().unwrap();
        let manager = crate::clawd::app_services::AppServiceManager::new(broker.clone());
        state.install_app_service_manager(&manager).unwrap();
        let service = crate::activities::open_default().unwrap();
        let activity = service.create(fixture.owner_uid, serde_json::from_value(json!({
            "title":"Real isolated Activity", "goal":"Keep effects separate from goal achievement"
        })).unwrap()).unwrap();
        let policy = |deny: bool| {
            serde_json::from_value(json!({"rules":[
                {"verb":"fs.write","mode":if deny {"deny"} else {"normal"},
                 "scopes":if deny {json!([])} else {json!([{"kind":"path","value":"/**"}])}}
            ]}))
            .unwrap()
        };
        if case != "policy_introduce" {
            service
                .set_capability_policy(
                    fixture.owner_uid,
                    &activity.id,
                    None,
                    policy(case == "deny"),
                )
                .unwrap();
        }
        let expires = chrono::Utc::now()
            + if case == "limit_expiry" {
                chrono::Duration::seconds(20)
            } else {
                chrono::Duration::hours(1)
            };
        service
            .set_execution_limits(
                fixture.owner_uid,
                &activity.id,
                None,
                crate::activities::ExecutionLimitsDraft {
                    max_attempts: 1,
                    max_turns_per_attempt: 3,
                    expires_at: expires.to_rfc3339(),
                },
            )
            .unwrap();
        let session = crate::paths::with_user_override(
            fixture.owner_uid,
            fixture.owner_home.clone(),
            async { crate::session::create("real isolated Activity fixture").unwrap() },
        )
        .await;
        crate::session::set_caps(&session, &fixture.caps).unwrap();
        crate::session::update_meta(&session, |meta| {
            meta.owner_uid = Some(fixture.owner_uid);
            meta.role = Some(Role::AgentHost);
            meta.activity_id = Some(activity.id.clone());
            meta.origin = Some(crate::session::SessionOrigin::SystemAgentTask);
        })
        .unwrap();
        let ready_file = fixture
            .owner_home
            .join(format!("activity-ready-{}.json", activity.id));
        let child_case = match case {
            "one_shot" => {
                json!({"case":"one_shot","operation":fixture.operation,"args":fixture.operation_args})
            }
            "stateful" => json!({"case":"stateful","tool":fixture.tool,
                "first_arguments":fixture.first_arguments,"second_arguments":fixture.second_arguments}),
            "deny" => {
                json!({"case":"denied","tool":fixture.tool,"arguments":fixture.first_arguments})
            }
            _ => json!({"case":"wait_for_stop","expected_turns":3,"ready_file":ready_file}),
        };
        let context = runtime.path().join("worker-context.json");
        std::fs::write(
            &context,
            serde_json::to_vec(&json!({
                "owner_uid":fixture.owner_uid,"execution_gid":broker.isolated_execution_gid,
                "owner_home":fixture.owner_home,"app_id":fixture.app_id,
                "package_digest":package.content_digest,"case":child_case
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&context, std::fs::Permissions::from_mode(0o644)).unwrap();
        let shim = runtime.path().join("claw-agentd");
        std::fs::write(&shim, format!(
            "#!/bin/sh\nexport COS_ACTIVITY_EXTENSION_PROCESS_CONTEXT={}\nexec {} --exact {} --ignored --nocapture --test-threads=1\n",
            quote(&context), quote(&std::env::current_exe().unwrap()), CHILD,
        )).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _worker = TestEnvVarGuard::set("COS_AGENTD_BIN", &shim);
        let store = Store::open_default().unwrap();
        let before = counter(&fixture.counter_file);
        let job = store
            .submit_with_activity(
                "real Host Activity fixture".into(),
                None,
                None,
                Some(session.to_string()),
                Some(20),
                false,
                Some(fixture.owner_uid),
                Some(fixture.owner_home.to_string_lossy().into_owned()),
                Some(activity.id.clone()),
            )
            .unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let sweeper = manager.spawn_sweeper(shutdown.clone());
        let supervisor = tokio::spawn(run_with_store_and_broker(
            SupervisorConfig {
                poll: Duration::from_millis(25),
                max_workers: 1,
                lease: Duration::from_secs(60),
                heartbeat_grace: Duration::from_secs(30),
                ..SupervisorConfig::default()
            },
            shutdown.clone(),
            store.clone(),
            broker,
        ));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        let mut changed = false;
        let finished = loop {
            if !changed && ready_file.is_file() {
                let ready: Value =
                    serde_json::from_slice(&std::fs::read(&ready_file).unwrap()).unwrap();
                assert_eq!(ready["worker_uid"], fixture.owner_uid);
                assert_ne!(ready["extension_uid"], fixture.owner_uid);
                assert_eq!(ready["max_turns"], 3);
                match case {
                    "limit_disable" => {
                        service
                            .set_execution_limits_enabled(fixture.owner_uid, &activity.id, 1, false)
                            .unwrap();
                    }
                    "limit_revision" => {
                        let mut limits = service
                            .execution_limits(fixture.owner_uid, &activity.id)
                            .unwrap()
                            .unwrap()
                            .limits;
                        limits.max_attempts = 2;
                        service
                            .set_execution_limits(fixture.owner_uid, &activity.id, Some(1), limits)
                            .unwrap();
                    }
                    "policy_disable" => {
                        service
                            .set_capability_policy_enabled(
                                fixture.owner_uid,
                                &activity.id,
                                1,
                                false,
                            )
                            .unwrap();
                    }
                    "policy_revision" | "policy_introduce" => {
                        service
                            .set_capability_policy(
                                fixture.owner_uid,
                                &activity.id,
                                (case == "policy_revision").then_some(1),
                                policy(true),
                            )
                            .unwrap();
                    }
                    _ => {}
                }
                changed = true;
            }
            let current = store.locate(&job.id).unwrap().unwrap().1;
            if matches!(
                current.status,
                JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled
            ) {
                break current;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{case}: real worker did not stop"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        shutdown.store(true, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(60), supervisor)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(60), sweeper)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(finished.schema_version, 2);
        assert_eq!(finished.recovery_count, 0, "{case}: no replay after COMMIT");
        assert_eq!(
            service
                .execution_limits(fixture.owner_uid, &activity.id)
                .unwrap()
                .unwrap()
                .used_attempts,
            1
        );
        assert_eq!(
            service.get(fixture.owner_uid, &activity.id).unwrap().state,
            ActivityState::Active
        );
        let receipts = service
            .receipts(fixture.owner_uid, &activity.id, 100)
            .unwrap();
        match case {
            "one_shot" | "stateful" => {
                assert_eq!(
                    finished.status,
                    JobStatus::Ok,
                    "{case}: {:?}",
                    finished.error
                );
                let calls = if case == "one_shot" { 1 } else { 2 };
                assert_eq!(counter(&fixture.counter_file), before + calls);
                assert_eq!(receipts.len(), calls as usize);
                assert!(receipts
                    .iter()
                    .all(|receipt| receipt.source == ReceiptSource::CallerReported
                        && receipt.report.app_id == fixture.app_id));
            }
            _ => {
                assert_eq!(
                    finished.status,
                    JobStatus::Error,
                    "{case}: {:?}",
                    finished.error
                );
                let reason = finished.error.as_deref().expect("constraint stop reason");
                let expected = match case {
                    "deny" => "denies",
                    "limit_expiry" => "expired",
                    "limit_disable" | "policy_disable" => "disabled",
                    _ => "changed",
                };
                assert!(reason.contains(expected), "{case}: {reason}");
                assert_eq!(counter(&fixture.counter_file), before);
                assert!(
                    crate::approvals::list_pending_for_owner(Some(fixture.owner_uid)).is_empty()
                );
                if case != "deny" {
                    assert!(changed, "{case}: stopped before a live worker was observed");
                    assert_eq!(
                        finished.execution_phase,
                        crate::agent::service::ExecutionPhase::Indeterminate
                    );
                }
            }
        }
        assert!(store.claim_one().unwrap().is_none());
        if ready_file.exists() {
            std::fs::remove_file(ready_file).unwrap();
        }
        drop(manager);
        drop(listener);
    }
}
