use super::*;
use crate::approvals::system_review::{self as reviews, ReviewKind, ReviewState};
use crate::bridge::{AppIdentitySession, LaunchRequest};
use crate::caps::{Cap, Scope, Verb};
use crate::test_env::{TestEnvVarGuard, TestSessionGuard};
use serde_json::{json, Value};
use std::cell::Cell;
use std::path::Path;

fn launch(root: &Path, app_id: &str, cap: Option<&Cap>) -> AppLaunch {
    let directory = root.join(app_id);
    std::fs::create_dir_all(&directory).unwrap();
    let needs = cap
        .map(|cap| {
            vec![json!({
                "verb": cap.verb,
                "scope": crate::caps::manifest::ScopeBinding::Fixed {
                    scope: cap.scope.clone(),
                },
                "why": {"en": "Inspect the requested hardware"},
            })]
        })
        .unwrap_or_default();
    std::fs::write(
        directory.join("app.json"),
        json!({
            "id": app_id,
            "version": "1.0.0",
            "schema_version": 2,
            "name": {"en": "Reviewed local fixture"},
            "runtime": "python",
            "desktop": {"exec": "--gui"},
            "operations": {
                "probe": {"label": {"en": "Probe"}, "stdin": true, "needs": needs},
            },
            "mcp": {
                "transport": "stdio",
                "entry": "server.py",
                "tools": [{
                    "name": format!("{app_id}.probe"),
                    "summary": {"en": "Probe"},
                    "needs": [],
                }],
            },
        })
        .to_string(),
    )
    .unwrap();
    for entry in ["main.py", "server.py"] {
        std::fs::write(directory.join(entry), "# review fixture; never executed\n").unwrap();
    }
    crate::test_env::app_launch(&directory, app_id)
}

fn refused<T>(result: Result<T, String>) -> String {
    match result {
        Ok(_) => panic!("unreviewed or denied local registration succeeded"),
        Err(error) => error,
    }
}

fn review_id(error: &str, app_id: &str) -> String {
    let value: Value =
        serde_json::from_str(error).unwrap_or_else(|_| panic!("lost structured review: {error}"));
    assert_eq!(value["code"], "not_authorized");
    assert_eq!(value["details"]["status"], "system_review_required");
    assert_eq!(value["details"]["app_id"], app_id);
    let id = value["details"]["system_review_id"].as_str().unwrap();
    assert!(id.starts_with("rv-"));
    id.to_string()
}

#[cfg(unix)]
#[test]
fn local_registration_checks_root_and_review_before_deriving_capabilities() {
    let _lock = crate::test_env::lock_env();
    let root = crate::test_env::secure_scratch_dir("local-registration-review");
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", &root);
    let _caps = TestEnvVarGuard::set("COS_CAPS_DATA_DIR", &root);
    let _fixture = TestEnvVarGuard::remove("COS_TEST_LOCAL_APP_SESSIONS");
    let _parent = TestSessionGuard::admin(&root);
    let launch = launch(&root, "local-review", None);
    let parent = crate::proc::current_session_info_for_caps().unwrap();
    let before = crate::proc::registry_sessions().len();
    let derived = Cell::new(false);
    let error = refused(AppIdentitySession::register_local(
        &parent,
        &launch,
        &LaunchRequest::Operation {
            operation: "probe",
            args: &[],
        },
        parent.caps.clone().unwrap(),
        |_| {
            derived.set(true);
            Ok(CapSet::new())
        },
    ));
    assert!(!derived.get());
    assert_eq!(crate::proc::registry_sessions().len(), before);
    if unsafe { libc::geteuid() } == 0 {
        review_id(&error, launch.app_id());
    } else {
        assert!(error.contains("requires the Root authority"), "{error}");
        assert!(!root.join("approvals").exists());
    }
    drop(_parent);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn non_root_launchers_keep_the_broker_and_cannot_select_local_owner_authority() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let _lock = crate::test_env::lock_env();
    let _fixture = TestEnvVarGuard::remove("COS_TEST_LOCAL_APP_SESSIONS");
    let root = tempfile::tempdir().unwrap();
    let _private = TestEnvVarGuard::remove(crate::extension_host::protocol::BROKER_SOCKET_ENV);
    assert!(use_clawd_backend().unwrap());
    let _host = TestEnvVarGuard::set(
        crate::extension_host::protocol::BROKER_SOCKET_ENV,
        root.path().join("private.sock"),
    );
    assert!(use_clawd_backend().unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(crate::paths::with_user_override(
        42_424,
        root.path().to_path_buf(),
        async {
            let error = use_clawd_backend().unwrap_err();
            assert!(error.contains("owner-overridden App launches"), "{error}");
        },
    ));
    let _local_fixture = TestEnvVarGuard::set("COS_TEST_LOCAL_APP_SESSIONS", "1");
    assert!(!use_clawd_backend().unwrap());
}

#[test]
fn registration_cannot_review_one_package_and_name_a_different_app() {
    let _lock = crate::test_env::lock_env();
    let root = crate::test_env::secure_scratch_dir("local-review-identity");
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", &root);
    let _caps = TestEnvVarGuard::set("COS_CAPS_DATA_DIR", &root);
    let _fixture = TestEnvVarGuard::set("COS_TEST_LOCAL_APP_SESSIONS", "1");
    let launch = launch(&root, "local-review", None);
    for error in [
        refused(AppIdentitySession::for_operation(
            &launch,
            "another-app",
            "probe",
            &[],
        )),
        refused(AppIdentitySession::for_gui(&launch, "another-app", "--gui")),
    ] {
        assert!(error.contains("does not match verified App"), "{error}");
    }
    assert!(!root.join("approvals").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_review_refusal_keeps_the_broker_code_and_private_payload() {
    let details = json!({
        "status": "system_review_required",
        "system_review_id": "rv-0123456789abcdef0123456789abcdef",
        "app_id": "local-review",
    });
    let error = crate::clawd::protocol::BrokerError::authorization_required(
        "waiting for OS review",
        details.clone(),
    )
    .classified("system_review_required");
    let error = ClawdCallError::from(error);
    assert_eq!(error.code.as_deref(), Some("not_authorized"));
    assert_eq!(error.data.as_ref(), Some(&details));
    let encoded: Value = serde_json::from_str(&error.to_string()).unwrap();
    assert_eq!(encoded["details"], details);
    assert!(encoded.get("audit_class").is_none());
}

#[cfg(target_os = "linux")]
fn check_root_broker_registration(root: &Path, parent_caps: &CapSet) {
    use crate::clawd::protocol::{BrokerError, Request, Response};
    use crate::clawd::routes::Command;
    use crate::clawd::{transport::frame, wire};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};

    let cap = Cap::new(Verb::SYS_OBSERVE, Scope::name("hardware"));
    let launch = launch(root, "broker-review", Some(&cap));
    let granted = CapSet::from_caps([cap]);
    for private in [false, true] {
        let directory = root.join(if private { "private" } else { "ordinary" });
        std::fs::create_dir(&directory).unwrap();
        let _runtime = TestEnvVarGuard::set("COS_RUNTIME_DIR", &directory);
        let _no_private =
            TestEnvVarGuard::remove(crate::extension_host::protocol::BROKER_SOCKET_ENV);
        let public_socket = crate::paths::clawd_socket_path();
        let _private = private.then(|| {
            TestEnvVarGuard::set(
                crate::extension_host::protocol::BROKER_SOCKET_ENV,
                directory.join("host.sock"),
            )
        });
        let socket = crate::paths::clawd_socket_path();
        assert_eq!(socket != public_socket, private);
        let public_listener = private.then(|| UnixListener::bind(&public_socket).unwrap());
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let package = serde_json::to_value(launch.package_ref()).unwrap();
        let reply_caps = serde_json::to_value(&granted).unwrap();
        let proc_dir = directory.join("proc");
        let required = json!({
            "status": "system_review_required",
            "system_review_id": "rv-0123456789abcdef0123456789abcdef",
            "app_id": launch.app_id(),
        });
        let review = required.clone();
        let server = std::thread::spawn(move || {
            let mut registrations = Vec::new();
            for (index, command) in [
                Command::AppSessionRegister,
                Command::AppSessionRegister,
                Command::PermissionStatus,
                Command::AppSessionRegister,
                Command::AppSessionDeregister,
            ]
            .into_iter()
            .enumerate()
            {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "missing broker request {command}"
                            );
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("accept fixture connection: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut header = [0_u8; wire::HEADER_BYTES];
                stream.read_exact(&mut header).unwrap();
                let length =
                    frame::parse_header(&header, wire::KIND_REQUEST, wire::MAX_REQUEST_BYTES)
                        .unwrap();
                let mut body = vec![0_u8; length];
                stream.read_exact(&mut body).unwrap();
                let request: Request = serde_json::from_slice(&body).unwrap();
                assert_eq!(
                    request.command, command,
                    "unexpected review polling or route"
                );
                if command == Command::AppSessionRegister {
                    assert_eq!(request.params["package"], package);
                    assert_eq!(request.params["app_id"], "broker-review");
                    assert_eq!(request.params["operation"], "probe");
                    for forbidden in ["reviewed", "system_review_id", "owner_uid", "session_id"] {
                        assert!(request.params.get(forbidden).is_none());
                    }
                    registrations.push(request.params.clone());
                }
                let response = match index {
                    0 => Response::handler_error(
                        request.id,
                        BrokerError::authorization_required("OS review required", review.clone()),
                    ),
                    1 => Response::handler_error(
                        request.id,
                        BrokerError::authorization_required(
                            "capability approval required",
                            json!({
                                "status": "approval_required",
                                "approval_requests": ["ap-guard"],
                            }),
                        ),
                    ),
                    2 => {
                        assert_eq!(request.params["ids"], json!(["ap-guard"]));
                        Response::ok(
                            request.id,
                            json!({"statuses": [{"id": "ap-guard", "status": "approved"}]}),
                        )
                    }
                    3 => Response::ok(
                        request.id,
                        json!({
                            "session_id": "app-broker-fixture",
                            "proc_data_dir": proc_dir,
                            "handle": "fixture-handle",
                            "caps": reply_caps,
                        }),
                    ),
                    4 => {
                        assert_eq!(request.params["session_id"], "app-broker-fixture");
                        assert_eq!(request.params["handle"], "fixture-handle");
                        Response::ok(request.id, json!({"ok": true}))
                    }
                    _ => unreachable!(),
                };
                stream
                    .write_all(&frame::encode_frame(
                        wire::KIND_RESPONSE,
                        &serde_json::to_vec(&response).unwrap(),
                    ))
                    .unwrap();
            }
            assert_eq!(registrations.len(), 3);
            assert!(registrations.windows(2).all(|pair| pair[0] == pair[1]));
        });
        let register = || {
            AppIdentitySession::register_with_clawd(
                launch.app_id(),
                &LaunchRequest::Operation {
                    operation: "probe",
                    args: &[],
                },
                parent_caps.clone(),
                CapSet::new(),
                Some(&launch.ceiling()),
                &launch.package_ref(),
            )
        };
        let denial = refused(register());
        assert_eq!(
            serde_json::from_str::<Value>(&denial).unwrap()["details"],
            required
        );
        let session = register().unwrap();
        assert!(!session.uses_local_backend());
        assert_eq!(session.granted_caps(), &granted);
        assert_eq!(session.package, launch.package_ref());
        drop(session);
        server.join().unwrap();
        if let Some(listener) = public_listener {
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "private registration escaped to the ordinary socket"
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires UID 0; isolated temporary review store and registry, no App execution"]
fn root_local_registration_requires_protected_owner_review() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "run this fixture as Root");
    let _lock = crate::test_env::lock_env();
    // The routed registry has a fixed production path. Never expose live /run.
    assert_eq!(
        unsafe { libc::unshare(libc::CLONE_NEWNS) },
        0,
        "private mount namespace: {}",
        std::io::Error::last_os_error()
    );
    assert_eq!(
        unsafe {
            libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            )
        },
        0,
        "private mount propagation: {}",
        std::io::Error::last_os_error()
    );
    assert_eq!(
        unsafe {
            libc::mount(
                c"tmpfs".as_ptr(),
                c"/run".as_ptr(),
                c"tmpfs".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                c"mode=755".as_ptr().cast(),
            )
        },
        0,
        "isolated runtime root: {}",
        std::io::Error::last_os_error()
    );
    let root = tempfile::Builder::new()
        .prefix("claw-local-review-")
        .tempdir_in("/run")
        .unwrap();
    let _home = TestEnvVarGuard::set("HOME", root.path());
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let caps_root = root.path().join("caps");
    let _caps = TestEnvVarGuard::set("COS_CAPS_DATA_DIR", &caps_root);
    let _runtime = TestEnvVarGuard::set("COS_RUNTIME_DIR", root.path().join("no-daemon"));
    let _broker = TestEnvVarGuard::set(
        crate::extension_host::protocol::BROKER_SOCKET_ENV,
        root.path().join("no-broker.sock"),
    );
    let _fixture = TestEnvVarGuard::remove("COS_TEST_LOCAL_APP_SESSIONS");
    let owner = 42_424;
    let other_owner = 42_425;
    let _parent = TestSessionGuard::admin(&Path::new("/run/cos/caps").join(owner.to_string()));
    let original = launch(root.path(), "local-review", None);
    let before = crate::proc::registry_sessions().len();
    let parent = crate::proc::current_session_info_for_caps().unwrap();
    let derived = Cell::new(false);
    let root_review = review_id(
        &refused(AppIdentitySession::register_local(
            &parent,
            &original,
            &LaunchRequest::Operation {
                operation: "probe",
                args: &[],
            },
            parent.caps.clone().unwrap(),
            |_| {
                derived.set(true);
                Ok(CapSet::new())
            },
        )),
        original.app_id(),
    );
    assert!(!derived.get());
    assert_eq!(reviews::get(0, &root_review).unwrap().owner_uid, 0);
    assert_eq!(crate::proc::registry_sessions().len(), before);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(crate::paths::with_user_override(
        owner,
        root.path().join("owner-home"),
        async {
            assert!(!use_clawd_backend().unwrap());
            let operation =
                || AppIdentitySession::for_operation(&original, original.app_id(), "probe", &[]);
            let error = refused(operation());
            let pending = review_id(&error, original.app_id());
            let gui = refused(AppIdentitySession::for_gui(
                &original,
                original.app_id(),
                "--gui",
            ));
            let mcp = refused(AppIdentitySession::for_mcp(&original, "local-review.probe"));
            assert_eq!(review_id(&gui, original.app_id()), pending);
            assert_eq!(review_id(&mcp, original.app_id()), pending);
            let record = reviews::get(owner, &pending).unwrap();
            assert_eq!(record.owner_uid, owner);
            assert_eq!(record.package, original.package_ref());
            assert!(record.requester.contains(&parent.session_id));
            assert_eq!(crate::proc::registry_sessions().len(), before);
            assert_eq!(reviews::pending(owner, 100).unwrap().len(), 1);
            assert_eq!(reviews::pending(0, 100).unwrap()[0].id, root_review);

            reviews::decide(owner, &pending, false, None).unwrap();
            let denied_retry = review_id(&refused(operation()), original.app_id());
            assert_eq!(
                reviews::get(owner, &pending).unwrap().state,
                ReviewState::Denied
            );
            assert_eq!(crate::proc::registry_sessions().len(), before);
            let other = reviews::submit(
                other_owner,
                ReviewKind::AppActivation,
                original.package(),
                "different owner".into(),
            )
            .unwrap();
            reviews::decide(other_owner, &other.id, true, Some(original.package())).unwrap();
            reviews::consume(other_owner, &other.id, original.package()).unwrap();
            assert_eq!(
                review_id(&refused(operation()), original.app_id()),
                denied_retry
            );
            assert!(!reviews::has_accepted(owner, original.package()).unwrap());

            reviews::decide(owner, &denied_retry, true, Some(original.package())).unwrap();
            {
                let (operation, args) = operation().unwrap();
                let gui =
                    AppIdentitySession::for_gui(&original, original.app_id(), "--gui").unwrap();
                let mcp = AppIdentitySession::for_mcp(&original, "local-review.probe").unwrap();
                assert!(args.is_empty());
                for session in [&operation, &gui, &mcp] {
                    assert!(session.uses_local_backend());
                    assert!(session.granted_caps().is_empty());
                    assert_eq!(session.package, original.package_ref());
                    let row = crate::proc::session_info_by_id(session.id()).unwrap();
                    assert!(row.pending_bind);
                    assert_eq!(row.pid, 0);
                    assert_eq!(row.parent.as_deref(), Some(parent.session_id.as_str()));
                }
            }
            assert_eq!(crate::proc::registry_sessions().len(), before);
            assert_eq!(
                reviews::get(owner, &denied_retry).unwrap().state,
                ReviewState::Consumed
            );

            let cap = Cap::new(Verb::SYS_OBSERVE, Scope::name("hardware"));
            let changed = launch(
                &root.path().join("changed-contract"),
                "local-review",
                Some(&cap),
            );
            let changed_operation =
                || AppIdentitySession::for_operation(&changed, changed.app_id(), "probe", &[]);
            let changed_review = review_id(&refused(changed_operation()), changed.app_id());
            assert_ne!(changed_review, denied_retry);
            assert_eq!(crate::proc::registry_sessions().len(), before);
            reviews::decide(owner, &changed_review, true, Some(changed.package())).unwrap();
            crate::approvals::app_policy::revoke(owner, changed.app_id(), cap.clone()).unwrap();
            let error = refused(changed_operation());
            assert!(reviews::has_accepted(owner, changed.package()).unwrap());
            assert_eq!(
                error,
                crate::approvals::app_policy::require(
                    owner,
                    changed.app_id(),
                    &CapSet::from_caps([cap])
                )
                .unwrap_err()
            );
            assert_eq!(crate::proc::registry_sessions().len(), before);

            let mut nested = parent.clone();
            nested.client.source = crate::session::SessionSource::App;
            let nested_error =
                crate::proc::with_trusted_session_override(nested, async { refused(operation()) })
                    .await;
            assert!(
                nested_error.contains("cannot orchestrate App calls"),
                "{nested_error}"
            );
            let mut unprivileged = parent.clone();
            unprivileged.caps = Some(CapSet::new());
            let no_invoke = crate::proc::with_trusted_session_override(unprivileged, async {
                refused(operation())
            })
            .await;
            assert!(no_invoke.contains("cannot invoke"), "{no_invoke}");
            assert_eq!(crate::proc::registry_sessions().len(), before);

            crate::approvals::generations::revoke(&crate::approvals::RevocationScope::Owner {
                uid: Some(owner),
            })
            .unwrap();
            review_id(&refused(operation()), original.app_id());
            assert_eq!(crate::proc::registry_sessions().len(), before);
            std::fs::write(
                caps_root
                    .join("approvals/system-reviews")
                    .join(format!("{denied_retry}.json")),
                b"invalid protected review",
            )
            .unwrap();
            let unavailable = refused(operation());
            assert!(unavailable.contains("parse system review"), "{unavailable}");
            assert_eq!(crate::proc::registry_sessions().len(), before);
        },
    ));
    assert!(!root.path().join("no-broker.sock").exists());
    check_root_broker_registration(root.path(), parent.caps.as_ref().unwrap());
    assert_eq!(crate::proc::registry_sessions().len(), before);
}
