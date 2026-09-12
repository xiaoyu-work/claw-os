use super::*;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;

const PROBE_TEST: &str =
    "extension_host::spawn::app_data::tests::isolated_service_data_worker_probe";
const APP: &str = "storage-probe";

fn launch(app: &str) -> HostLaunchSpec {
    HostLaunchSpec {
        purpose: protocol::HostPurpose::AppService,
        lease_id: "a".repeat(32),
        authority_session_id: Some("data-host".into()),
        app_id: Some(app.into()),
        host_session_id: Some("data-host".into()),
        controller_uid: 0,
        controller_gid: 0,
        controller_pid: std::process::id(),
        controller_start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        package: Some(PackageRef {
            kind: crate::provenance::PackageKind::App,
            id: app.into(),
            content_digest: "b".repeat(64),
            publisher_key_id: None,
            tier: "vendor".into(),
        }),
    }
}

#[test]
fn only_bound_app_services_select_a_persistent_partition() {
    let mut spec = launch(APP);
    assert_eq!(service_app(&spec).unwrap(), Some(APP));
    spec.package.as_mut().unwrap().id = "another-app".into();
    assert!(service_app(&spec).is_err());
    spec = launch("../escape");
    assert!(service_app(&spec).is_err());
    spec = launch(APP);
    spec.package = None;
    assert!(service_app(&spec).is_err());
    spec = launch(APP);
    spec.app_id = None;
    assert!(service_app(&spec).is_err());
    spec.purpose = protocol::HostPurpose::Task;
    assert_eq!(service_app(&spec).unwrap(), None);
}

#[test]
fn submount_detection_preserves_component_and_kernel_escape_boundaries() {
    let mount = b"1 0 0:1 / /data/app rw - ext4 /dev/fixture rw\n";
    check_mountinfo(Path::new("/data/app"), mount).unwrap();
    let sibling = b"2 1 0:2 / /data/application/cache rw - tmpfs tmpfs rw\n";
    check_mountinfo(Path::new("/data/app"), sibling).unwrap();
    let child = b"3 1 0:3 / /data/app/cache rw - tmpfs tmpfs rw\n";
    assert!(check_mountinfo(Path::new("/data/app"), child).is_err());
    let escaped = b"4 1 0:4 / /data/app\\040space/cache rw - tmpfs tmpfs rw\n";
    assert!(check_mountinfo(Path::new("/data/app space"), escaped).is_err());
    assert!(check_mountinfo(Path::new("/data/app"), b"").is_err());
    assert!(check_mountinfo(Path::new("/data/app"), b"bad record\n").is_err());
}

fn mount_tmpfs(target: &CStr) {
    assert_eq!(
        unsafe {
            libc::mount(
                c"tmpfs".as_ptr(),
                target.as_ptr(),
                c"tmpfs".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                c"mode=0755,size=1073741824".as_ptr().cast(),
            )
        },
        0,
        "private fixture mount: {}",
        std::io::Error::last_os_error()
    );
}

fn chown(path: &Path, uid: u32, gid: u32) {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(path.as_ptr(), uid, gid) }, 0);
}

fn copy_executable(source: &Path, target: &Path) {
    std::fs::copy(source, target).unwrap();
    std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755)).unwrap();
    chown(target, 0, 0);
}

const SERVER: &str = r#"
import json, os, pathlib, socket, sqlite3, sys

root = pathlib.Path(os.environ["COS_DATA_DIR"])
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {},
                  "serverInfo": {"name": "storage-probe", "version": "0.1.0"}}
    elif method == "tools/call":
        assert not pathlib.Path(sys.argv[1]).exists(), "another App entered the sandbox"
        assert not pathlib.Path(sys.argv[2]).exists(), "owner state entered the sandbox"
        try:
            socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        except OSError:
            pass
        else:
            raise AssertionError("direct network became available")
        with sqlite3.connect(root / "state.db") as db:
            db.execute("PRAGMA journal_mode=WAL")
            db.execute("CREATE TABLE IF NOT EXISTS counter (id INTEGER PRIMARY KEY, value INTEGER)")
            db.execute("INSERT OR IGNORE INTO counter VALUES (1, 0)")
            db.execute("UPDATE counter SET value=value+1 WHERE id=1")
            value = db.execute("SELECT value FROM counter WHERE id=1").fetchone()[0]
        result = {"content": [{"type": "text", "text": str(value)}], "isError": False}
    else:
        result = {"tools": []}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
"#;

#[test]
#[ignore = "private subprocess of persistent_service_data_survives_uid_and_host_replacement"]
fn isolated_service_data_worker_probe() {
    assert_eq!(std::env::var("COS_APP_DATA_TEST_PROBE").unwrap(), "1");
    assert_ne!(unsafe { libc::geteuid() }, 0);
    let app = std::env::var("COS_APP_DATA_TEST_APP").unwrap();
    let package = PathBuf::from(std::env::var("COS_APP_DATA_TEST_PACKAGE").unwrap());
    let foreign = std::env::var("COS_APP_DATA_TEST_FOREIGN").unwrap();
    let owner_state = std::env::var("COS_APP_DATA_TEST_OWNER_STATE").unwrap();
    let expected: u64 = std::env::var("COS_APP_DATA_TEST_EXPECTED")
        .unwrap()
        .parse()
        .unwrap();
    let root = crate::paths::data_dir();
    assert_eq!(root, crate::paths::user_data_dir());
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    assert_ne!(root, home);
    for key in ["COS_LOG_DIR", "COS_CACHE_DIR"] {
        assert!(PathBuf::from(std::env::var_os(key).unwrap()).starts_with(&home));
    }
    assert!(crate::worker::derive::app_partition(&root, "foreign-app").is_err());
    let caps = crate::caps::CapSet::new();
    let policy = crate::worker::derive::app_session(crate::worker::derive::AppSessionInput {
        app_id: &app,
        app_dir: &package,
        program: "/usr/bin/python3".into(),
        argv: vec![
            package.join("server.py").to_str().unwrap().into(),
            foreign,
            owner_state,
        ],
        caps: &caps,
        authorized_mounts: &[],
        lifetime: crate::worker::derive::SessionLifetime::Reusable,
        session_id: "data-worker-probe",
        data_dir: root.to_str().unwrap(),
        apps_dir: package.parent().unwrap().to_str().unwrap(),
        extra_env: BTreeMap::new(),
        package_identity: None,
        pinned_entries: vec![],
        transports: &[],
    })
    .unwrap();
    assert_eq!(policy.workdir, root.join("apps").join(&app));
    assert!(policy
        .mounts
        .iter()
        .filter(|mount| mount.class == crate::worker::MountClass::AppData)
        .all(|mount| mount.source == root.join("apps").join(&app)));
    let launch = crate::worker::WorkerLaunch::new(policy).with_authority(
        crate::worker::BrokerAuthority::new(
            "data-worker-probe",
            Some(app),
            caps,
            crate::worker::relay_slot(),
        ),
    );
    let mut prepared = crate::worker::prepare(&launch).unwrap();
    let mut child = prepared
        .command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    for (id, method) in [(1, "initialize"), (2, "tools/call")] {
        writeln!(
            input,
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": method,
                "params": {"name": "data.bump", "arguments": {}}
            })
        )
        .unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        assert_ne!(output.read_line(&mut line).unwrap(), 0);
        let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id);
        if id == 2 {
            assert_eq!(reply["result"]["content"][0]["text"], expected.to_string());
        }
    }
    drop(input);
    child.kill().unwrap();
    child.wait().unwrap();
    drop(prepared.resources);
    let mut release = [0_u8; 1];
    std::io::stdin().read_exact(&mut release).unwrap();
}

#[test]
#[ignore = "requires Root in a private mount namespace and COS_APP_DATA_TEST_COS"]
fn persistent_service_data_survives_uid_and_host_replacement() {
    let _lock = crate::test_env::lock_env();
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert_ne!(
        std::fs::read_link("/proc/self/ns/mnt").unwrap(),
        std::fs::read_link("/proc/1/ns/mnt").unwrap(),
        "never replace runtime files outside a private mount namespace"
    );
    let uid = std::fs::metadata(env!("CARGO_MANIFEST_DIR")).unwrap().uid();
    let owner = crate::agentd::spawn::resolve_identity(uid).unwrap();
    let source = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    chown(source.path(), owner.uid, owner.gid);
    let data_root = source.path().join("owner-data");
    let _data = crate::test_env::TestEnvVarGuard::set("COS_USER_DATA_DIR", &data_root);
    let canonical = crate::paths::RoutedPathContext::for_owner(owner.uid, owner.home.clone())
        .scope_sync(|| crate::worker::derive::app_partition(&data_root, APP))
        .unwrap();
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(owner.uid).unwrap();
        let db = rusqlite::Connection::open(canonical.join("state.db")).unwrap();
        db.execute_batch(
            "CREATE TABLE counter (id INTEGER PRIMARY KEY, value INTEGER);
            INSERT INTO counter VALUES (1, 40);",
        )
        .unwrap();
        std::fs::create_dir_all(data_root.join("apps/foreign-app")).unwrap();
        std::fs::write(data_root.join("apps/foreign-app/canary"), b"foreign App").unwrap();
        std::fs::write(data_root.join("owner-secret"), b"owner state").unwrap();
    }
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
        Path::new("/run/data-probe"),
    );
    let _runtime = crate::test_env::TestEnvVarGuard::set("COS_RUNTIME_DIR", "/run/data-runtime");
    std::fs::create_dir("/run/data-runtime").unwrap();
    for (execution_uid, expected) in [(300001, 41), (300003, 42)] {
        let execution_gid = execution_uid + 1;
        let extension = ExtensionIdentity {
            uid: execution_uid,
            gid: execution_gid,
            username: format!("data-probe-{execution_uid}"),
        };
        let paths = HostPaths::create(&owner).unwrap();
        paths.activate(execution_uid, execution_gid).unwrap();
        let data = Arc::new(
            prepare(&owner, &extension, &launch(APP), &paths)
                .unwrap()
                .unwrap(),
        );
        data.binding.require_current().unwrap();
        let package = paths.control_dir.join("package");
        std::fs::create_dir(&package).unwrap();
        std::fs::set_permissions(&package, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(package.join("server.py"), SERVER).unwrap();
        let mut command = std::process::Command::new("/run/data-probe");
        command.args([
            "--exact",
            PROBE_TEST,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]);
        command
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", &paths.control_dir)
            .env("XDG_RUNTIME_DIR", "/tmp")
            .env("COS_LOG_DIR", paths.control_dir.join("log"))
            .env("COS_CACHE_DIR", paths.control_dir.join("cache"))
            .env("COS_BIN", "/usr/local/bin/cos")
            .env("COS_APP_DATA_TEST_PROBE", "1")
            .env("COS_APP_DATA_TEST_APP", APP)
            .env("COS_APP_DATA_TEST_PACKAGE", &package)
            .env(
                "COS_APP_DATA_TEST_FOREIGN",
                data_root.join("apps/foreign-app/canary"),
            )
            .env(
                "COS_APP_DATA_TEST_OWNER_STATE",
                data_root.join("owner-secret"),
            )
            .env("COS_APP_DATA_TEST_EXPECTED", expected.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        data.configure_environment(&mut command);
        let task_path = CString::new(paths.dir.as_os_str().as_bytes()).unwrap();
        let child_data = data.clone();
        unsafe {
            command.pre_exec(move || {
                setup_private_mount_namespace(&task_path)?;
                child_data.install()?;
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(execution_gid) != 0
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
        let mut namespace =
            PrivateMountNamespace::capture(child.id(), &paths.dir, Some(data.destination.clone()))
                .unwrap();
        let binding = data.binding.clone();
        drop(data);
        child.stdin.take().unwrap().write_all(b"1").unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut timed_out = false;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                timed_out = true;
                break child.wait().unwrap();
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if expected == 41 {
            namespace.run_test_helper(true).unwrap();
            assert!(
                namespace.cleanup().is_err(),
                "a retained temporary mount must block cleanup"
            );
            namespace.run_test_helper(false).unwrap();
        }
        namespace.cleanup().unwrap();
        paths.cleanup().unwrap();
        assert!(!timed_out, "isolated App data probe timed out");
        assert!(status.success(), "{status}");
        assert!(!paths.dir.exists());
        binding.require_current().unwrap();
        let metadata = std::fs::metadata(canonical.join("state.db")).unwrap();
        assert_eq!((metadata.uid(), metadata.gid()), (owner.uid, owner.gid));
        let value: i64 = {
            let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(owner.uid).unwrap();
            rusqlite::Connection::open(canonical.join("state.db"))
                .unwrap()
                .query_row("SELECT value FROM counter WHERE id=1", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(value, expected);
    }
    assert_eq!(
        std::fs::read(data_root.join("owner-secret")).unwrap(),
        b"owner state"
    );
    assert_eq!(
        std::fs::read(data_root.join("apps/foreign-app/canary")).unwrap(),
        b"foreign App"
    );
}

#[test]
#[ignore = "requires Root in a private mount namespace"]
fn service_data_refuses_aliases_foreign_owners_submounts_and_replaced_bindings() {
    use std::os::unix::fs::symlink;

    let _lock = crate::test_env::lock_env();
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert_ne!(
        std::fs::read_link("/proc/self/ns/mnt").unwrap(),
        std::fs::read_link("/proc/1/ns/mnt").unwrap()
    );
    let uid = std::fs::metadata(env!("CARGO_MANIFEST_DIR")).unwrap().uid();
    let owner = crate::agentd::spawn::resolve_identity(uid).unwrap();
    let source = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    chown(source.path(), owner.uid, owner.gid);
    let data_root = source.path().join("owner-data");
    let runtime = source.path().join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_USER_DATA_DIR", &data_root);
    let _runtime = crate::test_env::TestEnvVarGuard::set("COS_RUNTIME_DIR", &runtime);
    let canonical = crate::paths::RoutedPathContext::for_owner(owner.uid, owner.home.clone())
        .scope_sync(|| crate::worker::derive::app_partition(&data_root, APP))
        .unwrap();
    std::fs::write(canonical.join("canary"), b"preserve").unwrap();
    chown(&canonical.join("canary"), owner.uid, owner.gid);
    let saved = data_root.join("apps/saved");
    let extension = ExtensionIdentity {
        uid: 300001,
        gid: 300002,
        username: "data-probe".into(),
    };
    let paths = HostPaths::create(&owner).unwrap();
    std::fs::rename(&canonical, &saved).unwrap();
    symlink(&saved, &canonical).unwrap();
    assert!(prepare(&owner, &extension, &launch(APP), &paths).is_err());
    assert_eq!(std::fs::read(saved.join("canary")).unwrap(), b"preserve");
    std::fs::remove_file(&canonical).unwrap();
    std::fs::rename(&saved, &canonical).unwrap();

    chown(&canonical, 300005, owner.gid);
    assert!(prepare(&owner, &extension, &launch(APP), &paths).is_err());
    assert_eq!(std::fs::metadata(&canonical).unwrap().uid(), 300005);
    chown(&canonical, owner.uid, owner.gid);

    let nested = canonical.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let target = CString::new(nested.as_os_str().as_bytes()).unwrap();
    mount_tmpfs(&target);
    assert!(prepare(&owner, &extension, &launch(APP), &paths).is_err());
    assert_eq!(unsafe { libc::umount2(target.as_ptr(), 0) }, 0);

    let data = prepare(&owner, &extension, &launch(APP), &paths)
        .unwrap()
        .unwrap();
    let binding = data.binding.clone();
    let destination = PathBuf::from(std::ffi::OsStr::from_bytes(data.destination.to_bytes()));
    let replaced = destination.with_file_name("retained-target");
    std::fs::rename(&destination, &replaced).unwrap();
    std::fs::create_dir(&destination).unwrap();
    assert_eq!(
        data.install().unwrap_err().raw_os_error(),
        Some(libc::ESTALE)
    );
    drop(data);
    paths.cleanup().unwrap();
    assert_eq!(
        std::fs::read(canonical.join("canary")).unwrap(),
        b"preserve"
    );

    std::fs::rename(&canonical, &saved).unwrap();
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(owner.uid).unwrap();
        std::fs::create_dir(&canonical).unwrap();
    }
    assert!(binding.require_current().is_err());
    assert_eq!(std::fs::read(saved.join("canary")).unwrap(), b"preserve");
}
