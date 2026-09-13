use super::*;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;

mod app_sources {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/support/app_sources.rs"
    ));
}
mod app_stage {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/support/app_stage.rs"
    ));
}

const SDK_ACTOR: &str = "clawd::calendar::tests::process::calendar_sdk_actor";

fn mount_tmpfs(path: &std::ffi::CStr) {
    assert_eq!(
        unsafe {
            libc::mount(
                c"tmpfs".as_ptr(),
                path.as_ptr(),
                c"tmpfs".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                c"mode=0755,size=1073741824".as_ptr().cast(),
            )
        },
        0,
        "private mount: {}",
        std::io::Error::last_os_error()
    );
}

fn own(path: &Path, uid: u32, gid: u32) {
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(path.as_ptr(), uid, gid) }, 0);
}

fn executable(source: &Path, target: &Path) {
    std::fs::copy(source, target).unwrap();
    std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755)).unwrap();
    own(target, 0, 0);
}

fn private_usr() {
    fn mirror(source: &Path, target: &Path, excluded: &[&str]) {
        std::fs::create_dir_all(target).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            if excluded.iter().any(|name| entry.file_name() == *name) {
                continue;
            }
            let destination = target.join(entry.file_name());
            let metadata = std::fs::symlink_metadata(entry.path()).unwrap();
            if metadata.is_symlink() {
                std::os::unix::fs::symlink(std::fs::read_link(entry.path()).unwrap(), destination)
                    .unwrap();
                continue;
            }
            if metadata.is_dir() {
                std::fs::create_dir(&destination).unwrap();
            } else {
                assert!(metadata.is_file());
                std::fs::File::create(&destination).unwrap();
            }
            bind(&entry.path(), &destination);
        }
    }
    fn bind(source: &Path, target: &Path) {
        let source = std::ffi::CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
        let target = std::ffi::CString::new(target.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(
            unsafe {
                libc::mount(
                    source.as_ptr(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND | libc::MS_REC,
                    std::ptr::null(),
                )
            },
            0,
            "private runtime mirror: {}",
            std::io::Error::last_os_error()
        );
    }
    let shadow = Path::new("/run/calendar-usr");
    mirror(Path::new("/usr"), shadow, &["lib", "libexec"]);
    mirror(Path::new("/usr/lib"), &shadow.join("lib"), &["cos"]);
    std::fs::create_dir_all(shadow.join("lib/cos/bin")).unwrap();
    std::fs::create_dir(shadow.join("libexec")).unwrap();
    bind(shadow, Path::new("/usr"));
}

#[tokio::test]
#[ignore = "private unprivileged child of calendar_mcp_to_broker_reader_and_public_sdk"]
async fn calendar_sdk_actor() {
    use claw_os_sdk::applet::{
        protocol::{ErrorCode, Failure},
        CalendarDate, Client, ClientError,
    };
    assert_eq!(std::env::var("COS_CALENDAR_ACTOR").unwrap(), "1");
    assert_ne!(unsafe { libc::geteuid() }, 0);
    std::io::stdin().read_exact(&mut [0_u8; 1]).unwrap();
    let client = Client::installed();
    let date = CalendarDate {
        year: 2026,
        month: 9,
        day: 9,
    };
    if std::env::var("COS_CALENDAR_CASE").unwrap() == "allowed" {
        let events = client.calendar_day(date).await.unwrap();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events
            .iter()
            .any(|event| event.title == "Calendar MCP event"));
        assert!(events.iter().any(|event| event.id == "wal-event"));
        assert!(!events.iter().any(|event| event.title.contains("decoy")));
    }
    assert!(matches!(
        client.calendar_day(date).await,
        Err(ClientError::Provider(Failure {
            code: ErrorCode::PermissionDenied,
            ..
        }))
    ));
    drop(client);
}

async fn create_mcp_event(
    app: &Path,
    runtime: &Path,
    data: &Path,
    owner: &crate::agentd::spawn::WorkerIdentity,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let gate = runtime.parent().unwrap().join("writer-policy");
    std::fs::write(&gate, br#"#!/usr/bin/python3
import json, sys
if sys.argv[1:] == ["--wire=1", "__policy", "check", "data.db.write", "--name", "calendar"]:
    value = {"ok": True, "wire_version": 1, "data": {"decision": "allow", "verb": "data.db.write", "scope": {"kind": "name", "value": "calendar"}}}
else:
    value = {"ok": False, "wire_version": 1, "code": "PERMISSION_DENIED", "error": "fixture has no memory or network authority"}
print(json.dumps(value))
"#).unwrap();
    std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = std::process::Command::new("/usr/bin/python3");
    command
        .arg(app.join("server.py"))
        .current_dir(app)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("PYTHONPATH", runtime)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("COS_APP_MANIFEST", app.join("app.json"))
        .env("COS_APP_ID", "calendar")
        .env("COS_SESSION", "writer-session")
        .env("COS_DATA_DIR", data)
        .env("CLAW_COS_BIN", &gate)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit());
    let (uid, gid) = (owner.uid, owner.gid);
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(gid) != 0
                || libc::setuid(uid) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    let call = serde_json::json!({
        "name": "calendar.create",
        "arguments": {"provider":"local", "title":"Calendar MCP event", "start":"2026-09-09", "end":"2026-09-10"},
        "_meta": {"claw-os.dev/call-context": {
            "wire_version": 1, "call_id": "create-event", "trace_id": "calendar-fixture",
            "session_id": "writer-session", "task_id": "fixture",
            "deadline_unix_ms": crate::agentd::grant::now_ms() + 20_000,
            "caller": {"kind":"system-agent", "id":"writer-session", "owner_uid": owner.uid},
        }}
    });
    for (id, method, params) in [
        (
            1,
            "initialize",
            serde_json::json!({"protocolVersion":"2025-06-18", "capabilities":{}, "clientInfo":{"name":"calendar-fixture","version":"0"}}),
        ),
        (2, "tools/call", call),
    ] {
        input
            .write_all(
                format!(
                    "{}\n",
                    serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let line = tokio::time::timeout(Duration::from_secs(10), output.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id, "{reply}");
        assert!(reply.get("error").is_none(), "{reply}");
        if id == 2 {
            assert!(
                !reply["result"]["isError"].as_bool().unwrap_or(false),
                "{reply}"
            );
            assert!(reply.to_string().contains("Calendar MCP event"), "{reply}");
        }
    }
    drop(input);
    child.kill().await.unwrap();
    child.wait().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires private Root namespaces, staged App source, and COS_CALENDAR_{COS,READER,PROVIDER} binaries"]
async fn calendar_mcp_to_broker_reader_and_public_sdk() {
    use crate::clawd::transport::{frame::PeerStream, peer, ReadOutcome};
    use crate::clawd::wire::Request;

    let _lock = crate::test_env::lock_env();
    assert_eq!(unsafe { libc::geteuid() }, 0);
    for namespace in ["mnt", "net"] {
        assert_ne!(
            std::fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap(),
            std::fs::read_link(format!("/proc/1/ns/{namespace}")).unwrap()
        );
    }
    let owner = crate::agentd::spawn::resolve_identity(
        std::fs::metadata(env!("CARGO_MANIFEST_DIR")).unwrap().uid(),
    )
    .unwrap();
    let source = app_sources::app_dir("calendar");
    let source_root = app_sources::source_root();
    let _git_count = crate::test_env::TestEnvVarGuard::set("GIT_CONFIG_COUNT", "1");
    let _git_key = crate::test_env::TestEnvVarGuard::set("GIT_CONFIG_KEY_0", "safe.directory");
    let _git_value = crate::test_env::TestEnvVarGuard::set("GIT_CONFIG_VALUE_0", &source_root);
    let stage = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    std::fs::set_permissions(stage.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let app = app_stage::app(&source, "product", "calendar", "calendar", stage.path());
    let runtime = app_stage::python_runtime(&source, stage.path());
    let user_data = stage.path().join("owner-data");
    std::fs::create_dir(&user_data).unwrap();
    own(&user_data, owner.uid, owner.gid);
    let partition = crate::paths::RoutedPathContext::for_owner(owner.uid, owner.home.clone())
        .scope_sync(|| crate::worker::derive::app_partition(&user_data, "calendar"))
        .unwrap();
    create_mcp_event(&app, &runtime, &partition, &owner).await;
    let database = partition.join("calendar/events.db");
    let connection = {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(owner.uid).unwrap();
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=WAL;")
            .unwrap();
        connection
            .execute(
                "INSERT INTO events (id,title,start_time,end_time,created_at,updated_at)
            VALUES ('wal-event','Live WAL event','2026-09-09','2026-09-10','fixture','fixture')",
                [],
            )
            .unwrap();
        connection
    };
    assert!(database.with_file_name("events.db-wal").exists());
    let before = std::fs::read(&database).unwrap();
    mount_tmpfs(c"/run");
    mount_tmpfs(c"/var/lib");
    private_usr();
    mount_tmpfs(c"/usr/local/bin");
    std::fs::create_dir_all("/run/cos").unwrap();
    executable(
        Path::new(&std::env::var("COS_CALENDAR_COS").unwrap()),
        Path::new("/usr/local/bin/cos"),
    );
    executable(
        Path::new(&std::env::var("COS_CALENDAR_READER").unwrap()),
        Path::new(READER),
    );
    executable(
        Path::new(&std::env::var("COS_CALENDAR_PROVIDER").unwrap()),
        Path::new("/usr/libexec/claw-os-applet-provider"),
    );
    executable(
        &std::env::current_exe().unwrap(),
        Path::new("/run/calendar-sdk-actor"),
    );
    let _root = crate::test_env::TestEnvVarGuard::set("COS_USER_DATA_DIR", &user_data);
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", "/run/calendar-state");
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", "/run/calendar-state");
    let _log = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", "/run/calendar-log");
    let _zone = crate::test_env::TestEnvVarGuard::set("TZ", "UTC");
    {
        let data = files::Database::open(&user_data, owner.uid)
            .unwrap()
            .unwrap();
        let view = files::QueryView::prepare(data).unwrap();
        let mount = view.mount();
        let directory = view.directory();
        {
            let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(owner.uid).unwrap();
            std::fs::write(partition.join("calendar/late-file"), b"live directory").unwrap();
            std::fs::write(user_data.join("outside-canary"), b"must not follow").unwrap();
            std::os::unix::fs::symlink(
                user_data.join("outside-canary"),
                partition.join("calendar/outside-link"),
            )
            .unwrap();
        }
        let mut probe = std::process::Command::new("/usr/bin/python3");
        probe.args(["-c", "import errno\nassert open('late-file').read() == 'live directory'\ntry:\n open('events.db', 'r+b')\nexcept OSError as e:\n assert e.errno == errno.EROFS, e\nelse:\n raise AssertionError('database mount is writable')\ntry:\n open('outside-link').read()\nexcept OSError as e:\n assert e.errno == errno.ELOOP, e\nelse:\n raise AssertionError('database view followed a symlink')"]);
        let (uid, gid) = (owner.uid, owner.gid);
        unsafe {
            probe.pre_exec(move || {
                files::install(mount, &directory)?;
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(gid) != 0
                    || libc::setuid(uid) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        assert!(
            probe.status().unwrap().success(),
            "pinned database view must be kernel-read-only"
        );
    }
    let decoy = stage.path().join("decoy");
    std::fs::create_dir_all(decoy.join("calendar")).unwrap();
    std::fs::write(
        decoy.join("calendar/events.db"),
        b"not the Calendar database",
    )
    .unwrap();
    for case in ["allowed", "wrong-scope"] {
        let mut command = std::process::Command::new("/run/calendar-sdk-actor");
        command
            .args([
                "--exact",
                SDK_ACTOR,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", &owner.home)
            .env("COS_DATA_DIR", &decoy)
            .env("COS_USER_DATA_DIR", &decoy)
            .env("COS_SESSION", "calendar-reader-session")
            .env("COS_PROC_DATA_DIR", format!("/run/cos/caps/{}", owner.uid))
            .env("COS_CALENDAR_ACTOR", "1")
            .env("COS_CALENDAR_CASE", case)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());
        let (uid, gid) = (owner.uid, owner.gid);
        unsafe {
            command.pre_exec(move || {
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(gid) != 0
                    || libc::setuid(uid) != 0
                    || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                crate::agentd::spawn::mark_inherited_descriptors_cloexec(3);
                Ok(())
            });
        }
        let mut actor = command.spawn().unwrap();
        let session = "calendar-reader-session";
        let caps = crate::caps::CapSet::from_caps([Cap::new(
            Verb::DATA_DB_READ,
            Scope::name(if case == "allowed" {
                "calendar"
            } else {
                "other"
            }),
        )]);
        let (_, grant) = authority()
            .issue(Issuance {
                issuer: Issuer::AppSessionAuthority,
                principal: Principal::of_process(owner.uid, actor.id()).unwrap(),
                binding: Binding::ProcessTree,
                subject: Subject::session(session),
                audience: AudienceSet::one(Audience::SystemService),
                caps: caps.clone(),
                lifetime: Duration::from_secs(30),
                uses: Uses::Budget(1),
                index_session: true,
            })
            .unwrap();
        let info = crate::proc::SessionInfo {
            session_id: session.into(),
            pid: actor.id(),
            command: vec!["calendar-sdk-actor".into()],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: Some("fixture".into()),
            parent: None,
            workdir: None,
            exit_code: None,
            ended_at: None,
            tier: None,
            scope: None,
            priority: None,
            caps: Some(caps),
            transient_caps: None,
            role: None,
            app_id: None,
            pending_bind: false,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(actor.id()),
            client: crate::session::SessionClient::default(),
        };
        crate::proc::register_session_for_owner(info, owner.uid).unwrap();
        let path = Path::new("/run/cos/clawd.sock");
        let listener = tokio::net::UnixListener::bind(path).unwrap();
        use std::os::fd::AsRawFd;
        peer::enable_credential_passing(listener.as_raw_fd()).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666)).unwrap();
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
                assert_eq!(process.uid, uid);
                let client = ClientIdentity::from_peer(process);
                let request: Request = serde_json::from_slice(&frame.body).unwrap();
                assert_eq!(
                    request.command,
                    crate::clawd::routes::Command::SystemCalendarDay
                );
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
        actor.stdin.take().unwrap().write_all(b"1").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        let status = loop {
            if let Some(status) = actor.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                actor.kill().unwrap();
                break actor.wait().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let sent = stop.send(());
        let served = broker.await;
        authority().revoke(grant.id);
        crate::proc::deregister_session_for_owner(session, owner.uid);
        std::fs::remove_file(path).unwrap();
        sent.unwrap();
        served.unwrap();
        assert!(status.success(), "{case}: {status}");
    }
    assert_eq!(std::fs::read(&database).unwrap(), before);
    assert_eq!(std::fs::metadata(&database).unwrap().uid(), owner.uid);
    assert_eq!(
        std::fs::read(decoy.join("calendar/events.db")).unwrap(),
        b"not the Calendar database"
    );
    drop(connection);
}

#[test]
fn calendar_database_paths_are_owner_bound_and_never_follow_aliases() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let uid = unsafe { libc::geteuid() };
    assert!(files::Database::open(root.path(), uid).unwrap().is_none());
    let directory = root.path().join("apps/calendar/calendar");
    std::fs::create_dir_all(&directory).unwrap();
    let database = directory.join("events.db");
    std::fs::write(&database, b"fixture").unwrap();
    assert!(files::Database::open(root.path(), uid).unwrap().is_some());
    let outside = root.path().join("outside");
    std::fs::write(&outside, b"untouched").unwrap();
    std::fs::remove_file(&database).unwrap();
    symlink(&outside, &database).unwrap();
    assert!(files::Database::open(root.path(), uid).is_err());
    std::fs::remove_file(&database).unwrap();
    std::fs::hard_link(&outside, &database).unwrap();
    assert!(files::Database::open(root.path(), uid).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
    std::fs::remove_file(&database).unwrap();
    std::fs::write(&database, b"fixture").unwrap();
    symlink(&outside, directory.join("events.db-wal")).unwrap();
    assert!(files::Database::open(root.path(), uid).is_err());
}
