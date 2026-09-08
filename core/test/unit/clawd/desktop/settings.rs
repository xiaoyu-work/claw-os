use super::*;

fn environment(runtime_dir: PathBuf) -> DesktopEnvironment {
    DesktopEnvironment {
        uid: unsafe { libc::geteuid() },
        gid: unsafe { libc::getegid() },
        home: PathBuf::from("/home/fixture"),
        runtime_dir,
        wayland_display: "wayland-1".into(),
        display: Some(":0".into()),
        xauthority: Some("/home/fixture/.Xauthority".into()),
        username: "fixture".into(),
    }
}

#[test]
fn settings_service_is_fixed_and_does_not_forward_authority_or_manager_environment() {
    let _lock = crate::test_env::lock_env();
    let _session = crate::test_env::TestEnvVarGuard::set("COS_SESSION", "untrusted-worker-session");
    let _bus =
        crate::test_env::TestEnvVarGuard::set("DBUS_SESSION_BUS_ADDRESS", "unix:path=/forged");
    let environment = environment(PathBuf::from("/run/user/1000"));
    let args = service_args(&["applications".into()], &environment).unwrap();
    assert!(args.contains(&"--user".into()));
    assert!(args.contains(&"--service-type=exec".into()));
    assert!(args.contains(&"--collect".into()));
    assert!(args.contains(&"--expand-environment=no".into()));
    assert!(args.contains(&"--property=ExecStartPost=/usr/bin/sleep 0.2".into()));
    assert!(!args.iter().any(|arg| arg == "--scope"
        || arg == "--wait"
        || arg.contains("COS_SESSION")
        || arg.contains("CLAWD_")
        || arg.contains("COS_MCP")
        || arg.contains("CLAW_COS_BIN")
        || arg.contains("NoNewPrivileges")
        || arg.contains("/forged")));
    let start = args.iter().position(|arg| arg == "--").unwrap();
    assert_eq!(&args[start + 1..start + 3], ["/usr/bin/env", "-i"]);
    assert_eq!(&args[args.len() - 2..], [PROGRAM, "applications"]);
    assert!(args.contains(&"DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus".into()));
    assert!(args.contains(&"PATH=/usr/sbin:/usr/bin:/sbin:/bin".into()));
    assert!(args.contains(&"HOME=/home/fixture".into()));
}

#[test]
fn settings_requires_a_real_owner_session_bus_and_refuses_symlinks() {
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let mut environment = environment(dir.path().to_path_buf());
    assert!(validate_session_bus(&environment)
        .unwrap_err()
        .contains("unavailable"));
    fs::write(dir.path().join("bus"), "not a socket").unwrap();
    assert!(validate_session_bus(&environment).is_err());
    fs::remove_file(dir.path().join("bus")).unwrap();
    let socket = std::os::unix::net::UnixListener::bind(dir.path().join("actual-bus")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("actual-bus"), dir.path().join("bus")).unwrap();
    assert!(validate_session_bus(&environment).is_err());
    fs::remove_file(dir.path().join("bus")).unwrap();
    fs::rename(dir.path().join("actual-bus"), dir.path().join("bus")).unwrap();
    assert!(validate_session_bus(&environment).is_err());
    fs::create_dir(dir.path().join("systemd")).unwrap();
    let _manager =
        std::os::unix::net::UnixListener::bind(dir.path().join("systemd/private")).unwrap();
    validate_session_bus(&environment).unwrap();
    environment.uid += 1;
    assert!(validate_session_bus(&environment).is_err());
    drop(socket);
}

struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct SessionManager {
    root: PathBuf,
    request_uid: u32,
    request_pid: u32,
    children: std::sync::Arc<std::sync::Mutex<Vec<ChildGuard>>>,
}

#[zbus::interface(name = "org.freedesktop.systemd1.Manager")]
impl SessionManager {
    async fn subscribe(&self) {}

    async fn unsubscribe(&self) {}

    async fn start_transient_unit(
        &self,
        name: String,
        mode: String,
        properties: Vec<(String, zbus::zvariant::OwnedValue)>,
        auxiliary: Vec<(String, Vec<(String, zbus::zvariant::OwnedValue)>)>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedObjectPath> {
        let uid = self.request_uid;
        let pid = self.request_pid;
        assert_eq!(uid, unsafe { libc::geteuid() });
        assert!(fs::read_to_string(format!("/proc/{pid}/status"))
            .unwrap()
            .lines()
            .any(|line| line == "NoNewPrivs:\t1"));
        assert_eq!(mode, "fail");
        assert!(auxiliary.is_empty());
        assert!(name.starts_with("claw-settings-") && name.ends_with(".service"));
        let properties = properties
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        let text =
            |key: &str| -> String { properties[key].try_clone().unwrap().try_into().unwrap() };
        assert_eq!(text("Type"), "exec");
        assert_eq!(text("CollectMode"), "inactive-or-failed");
        let post: Vec<(String, Vec<String>, bool)> = properties["ExecStartPost"]
            .try_clone()
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            post,
            vec![(
                "/usr/bin/sleep".into(),
                vec!["/usr/bin/sleep".into(), "0.2".into()],
                false
            )]
        );
        let start: Vec<(String, Vec<String>, Vec<String>)> = properties["ExecStartEx"]
            .try_clone()
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(start.len(), 1);
        let (program, mut args, flags) = start.into_iter().next().unwrap();
        assert_eq!(program, "/usr/bin/env");
        assert!(flags.contains(&"no-env-expand".into()));
        assert_eq!(&args[..2], ["/usr/bin/env", "-i"]);
        let target = args.iter().position(|arg| arg == PROGRAM).unwrap();
        assert_eq!(target, args.len() - 2);
        let page = args.last().unwrap().clone();
        assert!(["applications", "users", "sound"].contains(&page.as_str()));
        assert!(!args
            .iter()
            .any(|arg| arg.starts_with("COS_") || arg.starts_with("CLAW_")));
        assert!(args.contains(&format!(
            "DBUS_SESSION_BUS_ADDRESS=unix:path={}/bus",
            self.root.display()
        )));
        fs::write(
            self.root.join(format!("request-{page}.json")),
            serde_json::to_vec(&json!({"uid":uid,"pid":pid,"nnp":1,"args":args})).unwrap(),
        )
        .unwrap();

        // Substitute only in the private fake manager, never in the production
        // launcher. The independent harmless process observes the real exec env.
        args.truncate(target);
        args.extend(["/usr/bin/python3".into(), "-c".into(), r#"
import json,os,pathlib,sys,time
root=pathlib.Path(os.environ['HOME'])
page=sys.argv[1]
nnp=int(next(line.split(':')[1] for line in pathlib.Path('/proc/self/status').read_text().splitlines() if line.startswith('NoNewPrivs:')))
(root/('gui-'+page+'.json')).write_text(json.dumps({'uid':os.geteuid(),'pid':os.getpid(),'nnp':nnp,'env':dict(os.environ)}))
if page == 'applications':
    time.sleep(8)
sys.exit(1 if page == 'users' else 0)
"#.into(), page.clone()]);
        let mut command = std::process::Command::new(program);
        command
            .args(&args[1..])
            .env("COS_SESSION", "manager-authority-must-not-leak")
            .env("CLAW_COS_BIN", "/not-the-installed-cos")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn().unwrap();
        let children = std::sync::Arc::clone(&self.children);
        let index = {
            let mut children = children.lock().unwrap();
            let index = children.len();
            children.push(ChildGuard(child));
            index
        };
        let job_id = (index + 1) as u32;
        let job: zbus::zvariant::OwnedObjectPath =
            format!("/org/freedesktop/systemd1/job/{job_id}")
                .try_into()
                .unwrap();
        let connection = connection.clone();
        let signal_job = job.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let status = children.lock().unwrap()[index].0.try_wait().unwrap();
            let result = if status.is_some_and(|status| !status.success()) {
                "failed"
            } else {
                "done"
            };
            connection
                .emit_signal(
                    None::<&str>,
                    "/org/freedesktop/systemd1",
                    "org.freedesktop.systemd1.Manager",
                    "JobRemoved",
                    &(job_id, signal_job, name, result),
                )
                .await
                .unwrap();
        });
        Ok(job)
    }
}

const PROCESS_FIXTURE: &str =
    "clawd::desktop::settings::tests::settings_user_service_process_fixture";

fn fixture_process(role: &str, root: &Path, uid: u32, gid: u32) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", PROCESS_FIXTURE, "--test-threads=1"])
        .env_clear()
        .env("HOME", root)
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", root)
        .env("CLAW_SETTINGS_FIXTURE_ROLE", role)
        .env("CLAW_SETTINGS_FIXTURE_ROOT", root)
        .env("CLAW_SETTINGS_FIXTURE_UID", uid.to_string())
        .env("CLAW_SETTINGS_FIXTURE_GID", gid.to_string())
        .env("COS_DATA_DIR", root.join("data"));
    command
}

fn request_process(role: &str, root: &Path, uid: u32, gid: u32) -> Command {
    let mut command = fixture_process(role, root, uid, gid);
    command
        .env("COS_SESSION", "worker-authority-must-not-leak")
        .env("COS_MCP_SERVER", "1")
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/forged");
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}

#[test]
#[ignore = "requires root only to test owner-UID dropping against an isolated private session bus"]
fn settings_user_service_process_fixture() {
    if let Ok(role) = std::env::var("CLAW_SETTINGS_FIXTURE_ROLE") {
        let root = PathBuf::from(std::env::var_os("CLAW_SETTINGS_FIXTURE_ROOT").unwrap());
        let uid = std::env::var("CLAW_SETTINGS_FIXTURE_UID")
            .unwrap()
            .parse()
            .unwrap();
        let gid = std::env::var("CLAW_SETTINGS_FIXTURE_GID")
            .unwrap()
            .parse()
            .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            if role == "manager" {
                assert_eq!(
                    unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) },
                    0
                );
                let children = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                fs::create_dir(root.join("systemd")).unwrap();
                let listener =
                    tokio::net::UnixListener::bind(root.join("systemd/private")).unwrap();
                let mut connections = Vec::new();
                fs::write(root.join("manager-ready"), "").unwrap();
                while !root.join("finish").exists() {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let (stream, _) = accepted.unwrap();
                            let credentials = stream.peer_cred().unwrap();
                            let connection = zbus::connection::Builder::unix_stream(stream)
                                .server(zbus::Guid::generate()).unwrap().p2p()
                                .serve_at("/org/freedesktop/systemd1", SessionManager {
                                    root: root.clone(),
                                    request_uid: credentials.uid(),
                                    request_pid: credentials.pid().unwrap().try_into().unwrap(),
                                    children: std::sync::Arc::clone(&children),
                                }).unwrap().build().await.unwrap();
                            connections.push(connection);
                        }
                        _ = tokio::time::sleep(Duration::from_millis(20)) => {}
                    }
                }
                children.lock().unwrap().clear();
                drop(connections);
            } else {
                assert_eq!(
                    unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) },
                    1
                );
                let mut environment = environment(root.clone());
                environment.uid = uid;
                environment.gid = gid;
                environment.home = root;
                let page = if role == "unavailable" {
                    "applications"
                } else {
                    &role
                };
                let result = launch(vec![page.into()], environment).await;
                if role == "unavailable" {
                    assert!(result.unwrap_err().contains("service unavailable"));
                } else if role == "users" {
                    assert!(result.unwrap_err().contains("activation failed"));
                } else {
                    result.unwrap();
                }
            }
        });
        return;
    }
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "run only this fixture as root"
    );
    let metadata = fs::metadata(env!("CARGO_MANIFEST_DIR")).unwrap();
    let (uid, gid) = (metadata.uid(), metadata.gid());
    assert_ne!(
        uid, 0,
        "fixture requires a non-root-owned developer checkout"
    );
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let root = dir.path();
    let cpath = std::ffi::CString::new(root.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(cpath.as_ptr(), uid, gid) }, 0);
    let mut bus = Command::new("/usr/bin/dbus-daemon");
    bus.args(["--session", "--nofork"])
        .arg(format!("--address=unix:path={}/bus", root.display()))
        .env_clear()
        .env("HOME", root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        bus.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(gid) != 0
                || libc::setuid(uid) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let _bus = ChildGuard(bus.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("bus").exists() {
        assert!(
            Instant::now() < deadline,
            "private session bus did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut manager = fixture_process("manager", root, uid, gid);
    manager.stdout(Stdio::null()).stderr(Stdio::piped());
    unsafe {
        manager.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(gid) != 0
                || libc::setuid(uid) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut manager = ChildGuard(manager.spawn().unwrap());
    while !root.join("manager-ready").exists() {
        assert!(
            manager.0.try_wait().unwrap().is_none(),
            "private manager failed"
        );
        assert!(
            Instant::now() < deadline,
            "private session manager did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    for page in ["applications", "users", "sound"] {
        let output = request_process(page, root, uid, gid).output().unwrap();
        if !output.status.success() {
            let _ = manager.0.kill();
            let mut diagnostics = String::new();
            manager
                .0
                .stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut diagnostics)
                .unwrap();
            panic!(
                "request failed: {}\n{}\nmanager: {diagnostics}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let gui: Value =
            serde_json::from_slice(&fs::read(root.join(format!("gui-{page}.json"))).unwrap())
                .unwrap();
        assert_eq!(gui["uid"], uid);
        assert_eq!(
            gui["nnp"], 0,
            "the independent GUI must not inherit requester NNP"
        );
        assert!(gui["env"].get("COS_SESSION").is_none());
        assert!(gui["env"].get("COS_MCP_SERVER").is_none());
        assert!(gui["env"].get("CLAW_COS_BIN").is_none());
        if page == "applications" {
            assert!(
                Path::new(&format!("/proc/{}", gui["pid"])).exists(),
                "GUI lifetime must outlive the request process"
            );
        }
    }
    fs::write(root.join("finish"), "").unwrap();
    let status = manager.0.wait().unwrap();
    assert!(status.success());
    let output = request_process("unavailable", root, uid, gid)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
