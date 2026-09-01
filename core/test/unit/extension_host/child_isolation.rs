use super::*;

fn test_authority(roots: &[&Path]) -> IsolationAuthority {
    let approved_paths = roots
        .iter()
        .map(|root| {
            let canonical = root.canonicalize().unwrap();
            let metadata = fs::symlink_metadata(&canonical).unwrap();
            ApprovedPath {
                path: canonical.to_string_lossy().into_owned(),
                device: metadata.dev(),
                inode: metadata.ino(),
                owner_uid: metadata.uid(),
                mode: metadata.mode(),
            }
        })
        .collect();
    IsolationAuthority {
        task_id: "test-task".to_string(),
        session_id: Some("test-session".to_string()),
        capability_generation: "a".repeat(16),
        owner_uid: std::env::var("SUDO_UID")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| unsafe { libc::geteuid() as u32 }),
        extension_uid: unsafe { libc::geteuid() as u32 },
        execution_gid: 60_999,
        approved_paths,
    }
}

fn isolated_prepare(
    program: impl AsRef<OsStr>,
    initial_args: impl IntoIterator<Item = OsString>,
    authorized_root: Option<&Path>,
) -> Result<IsolatedLaunch, String> {
    let mut roots = authorized_root.into_iter().collect::<Vec<_>>();
    let sdk = std::env::var_os("COS_SDK_PYTHON_DIR").map(PathBuf::from);
    if let Some(sdk) = sdk.as_deref() {
        roots.push(sdk);
    }
    let authority = test_authority(&roots);
    prepare(program, initial_args, authorized_root, Some(&authority))
}

fn isolated_prepare_clean(
    program: impl AsRef<OsStr>,
    initial_args: impl IntoIterator<Item = OsString>,
    authorized_root: Option<&Path>,
    inner_env: Vec<(OsString, OsString)>,
) -> Result<IsolatedLaunch, String> {
    let roots = authorized_root.into_iter().collect::<Vec<_>>();
    let authority = test_authority(&roots);
    prepare_with_clean_env(
        program,
        initial_args,
        authorized_root,
        inner_env,
        Some(&authority),
    )
}

#[test]
fn disabled_child_isolation_preserves_the_original_command() {
    let _lock = crate::test_env::lock_env();
    let _guard = crate::test_env::TestEnvVarGuard::remove(ENABLE_ENV);
    let launch = isolated_prepare("python3", vec![OsString::from("-V")], None).unwrap();
    assert_eq!(launch.program, "python3");
    assert_eq!(launch.args, vec![OsString::from("-V")]);
}

#[test]
fn configured_inner_environment_rejects_loader_and_malformed_keys() {
    for key in [
        "LD_PRELOAD",
        "LD_AUDIT",
        "LD_LIBRARY_PATH",
        "bad-key",
        "9BAD",
    ] {
        let error = isolated_prepare_clean(
            "true",
            Vec::<OsString>::new(),
            None,
            vec![(key.into(), "value".into())],
        )
        .unwrap_err();
        assert!(error.contains("environment"), "{key}: {error}");
    }
}

#[test]
fn isolated_clean_environment_is_installed_after_bwrap_starts() {
    let _lock = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let _enabled = crate::test_env::TestEnvVarGuard::set(ENABLE_ENV, "1");
    let _home = crate::test_env::TestEnvVarGuard::set("HOME", home.path());
    let _proc = crate::test_env::TestEnvVarGuard::remove("COS_PROC_DATA_DIR");
    let _broker = crate::test_env::TestEnvVarGuard::remove("COS_EXTENSION_BROKER_SOCKET");
    let launch = isolated_prepare_clean(
        "true",
        Vec::<OsString>::new(),
        None,
        vec![("SAFE_VALUE".into(), "inside".into())],
    )
    .unwrap();
    assert!(launch.env.is_empty());
    let rendered = launch
        .args
        .iter()
        .map(|value| value.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(rendered.contains("--clearenv --setenv SAFE_VALUE inside"));
}

#[test]
fn typed_authority_is_mandatory_and_identity_environment_is_ignored() {
    let _lock = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let _enabled = crate::test_env::TestEnvVarGuard::set(ENABLE_ENV, "1");
    let _home = crate::test_env::TestEnvVarGuard::set("HOME", home.path());
    let _spoofed_uid =
        crate::test_env::TestEnvVarGuard::set("COS_EXTENSION_WORKER_UID", "61000");
    let _spoofed_gid =
        crate::test_env::TestEnvVarGuard::set("COS_EXTENSION_EXECUTION_GID", "1");
    let error = prepare("true", Vec::<OsString>::new(), None, None).unwrap_err();
    assert!(error.contains("typed runtime authority"), "{error}");
}

#[test]
fn arbitrary_srv_is_not_an_approved_extension_root() {
    if !Path::new("/srv").is_dir() {
        return;
    }
    let authority = test_authority(&[]);
    let error = authority.authorize_root(Path::new("/srv")).unwrap_err();
    assert!(error.contains("outside broker-approved"), "{error}");
}

#[test]
fn reserved_uid_and_execution_gid_objects_are_rejected_from_owner_snapshots() {
    use std::os::unix::ffi::OsStrExt;

    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    for (uid, gid) in [(EXTENSION_UID_START, 0), (0, 60_999)] {
        let _lock = crate::test_env::lock_env();
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let sentinel = source.path().join("sentinel");
        fs::write(&sentinel, b"secret").unwrap();
        let raw = std::ffi::CString::new(sentinel.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::chown(raw.as_ptr(), uid, gid) }, 0);
        let _enabled = crate::test_env::TestEnvVarGuard::set(ENABLE_ENV, "1");
        let _home = crate::test_env::TestEnvVarGuard::set("HOME", home.path());
        let _proc = crate::test_env::TestEnvVarGuard::remove("COS_PROC_DATA_DIR");
        let _broker =
            crate::test_env::TestEnvVarGuard::remove("COS_EXTENSION_BROKER_SOCKET");
        let error = isolated_prepare(
            "python3",
            vec!["-c".into(), "print('should-not-run')".into()],
            Some(source.path()),
        )
        .unwrap_err();
        assert!(error.contains("unsafe ownership or mode"), "{uid}:{gid}: {error}");
    }
}

#[test]
fn broker_approved_path_inode_substitution_is_rejected() {
    let parent = tempfile::tempdir().unwrap();
    let approved = parent.path().join("approved");
    fs::create_dir(&approved).unwrap();
    let authority = test_authority(&[&approved]);
    fs::rename(&approved, parent.path().join("old")).unwrap();
    fs::create_dir(&approved).unwrap();
    let error = authority.authorize_root(&approved).unwrap_err();
    assert!(error.contains("identity changed"), "{error}");
}

#[test]
fn isolated_child_gets_private_proc_and_an_empty_allowlisted_root() {
    let _lock = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("tool.py"), b"print('ok')").unwrap();
    let _enabled = crate::test_env::TestEnvVarGuard::set(ENABLE_ENV, "1");
    let _home =
        crate::test_env::TestEnvVarGuard::set("HOME", home.path().to_string_lossy().as_ref());
    let _proc = crate::test_env::TestEnvVarGuard::remove("COS_PROC_DATA_DIR");
    let _broker = crate::test_env::TestEnvVarGuard::remove("COS_EXTENSION_BROKER_SOCKET");
    let launch = isolated_prepare(
        "python3",
        vec![OsString::from(source.path().join("tool.py"))],
        Some(source.path()),
    )
    .unwrap();
    assert_eq!(launch.program, "/usr/bin/bwrap");
    let rendered = launch
        .args
        .iter()
        .map(|value| value.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    for required in [
        "--unshare-pid",
        "--unshare-net",
        "--proc /proc",
        "--tmpfs /",
        "--dir /usr",
    ] {
        assert!(rendered.contains(required), "{rendered}");
    }
    assert!(!rendered.contains("--ro-bind /usr /usr"), "{rendered}");
    for hidden in [
        " --ro-bind /home /home",
        " --ro-bind /mnt /mnt",
        " --ro-bind /var /var",
    ] {
        assert!(!rendered.contains(hidden), "{rendered}");
    }
}

#[test]
fn authorized_snapshot_rejects_symlinks() {
    use std::os::unix::fs::symlink;

    let _lock = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    symlink("/etc/passwd", source.path().join("escape")).unwrap();
    let _enabled = crate::test_env::TestEnvVarGuard::set(ENABLE_ENV, "1");
    let _home =
        crate::test_env::TestEnvVarGuard::set("HOME", home.path().to_string_lossy().as_ref());
    let _proc = crate::test_env::TestEnvVarGuard::remove("COS_PROC_DATA_DIR");
    let _broker = crate::test_env::TestEnvVarGuard::remove("COS_EXTENSION_BROKER_SOCKET");
    assert!(
        isolated_prepare("python3", Vec::<OsString>::new(), Some(source.path()))
            .unwrap_err()
            .contains("symlink")
    );
}

#[test]
fn hostile_siblings_get_private_proc_and_cannot_see_host_mounts() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::ffi::OsStrExt;
    use std::process::Stdio;

    if unsafe { libc::geteuid() } != 0 || !Path::new("/usr/bin/bwrap").exists() {
        return;
    }
    let _lock = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let sentinel_root = tempfile::tempdir().unwrap();
    for index in 0..64u32 {
        let path = sentinel_root.path().join(format!("pool-owned-{index}"));
        fs::write(&path, b"secret").unwrap();
        let raw = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe { libc::chown(raw.as_ptr(), 61_000 + index, 60_999) },
            0
        );
    }
    let sentinel = sentinel_root.path().join("pool-owned-0");

    let mounted = tempfile::tempdir().unwrap();
    let mount_raw = std::ffi::CString::new(mounted.path().as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::mount(
                c"tmpfs".as_ptr(),
                mount_raw.as_ptr(),
                c"tmpfs".as_ptr(),
                0,
                c"size=4096".as_ptr().cast(),
            )
        },
        0
    );
    let mounted_sentinel = mounted.path().join("pool-owned-mounted");
    fs::write(&mounted_sentinel, b"secret").unwrap();
    let mounted_file_raw = std::ffi::CString::new(mounted_sentinel.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe { libc::chown(mounted_file_raw.as_ptr(), 61_000, 60_999) },
        0
    );

    let _enabled = crate::test_env::TestEnvVarGuard::set(ENABLE_ENV, "1");
    let _home =
        crate::test_env::TestEnvVarGuard::set("HOME", home.path().to_string_lossy().as_ref());
    let _proc = crate::test_env::TestEnvVarGuard::remove("COS_PROC_DATA_DIR");
    let _broker = crate::test_env::TestEnvVarGuard::remove("COS_EXTENSION_BROKER_SOCKET");
    let first_script = "import os,time; print(os.getpid(), flush=True); time.sleep(30)";
    let first_launch = isolated_prepare(
        "python3",
        vec![OsString::from("-c"), OsString::from(first_script)],
        None,
    )
    .unwrap();
    let mut first = std::process::Command::new(first_launch.program)
        .args(first_launch.args)
        .envs(first_launch.env)
        .env("SIBLING_SECRET", "first-only")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let first_outer_pid = first.id();
    let mut first_stdout = BufReader::new(first.stdout.take().unwrap());
    let mut line = String::new();
    first_stdout.read_line(&mut line).unwrap();
    assert!(!line.trim().is_empty());

    let probe = format!(
        r#"import json,os
seen=[]
for name in os.listdir('/proc'):
    if name.isdigit():
        try:
            data=open('/proc/'+name+'/environ','rb').read()
            if b'first-only' in data: seen.append(name)
        except OSError: pass
def readable(path):
    try:
        open(path,'rb').read(1)
        return True
    except OSError:
        return False
try:
    os.kill({first_outer_pid}, 0)
    signalled=True
except OSError:
    signalled=False
fds=[]
for fd in os.listdir('/proc/self/fd'):
    try: fds.append(os.readlink('/proc/self/fd/'+fd))
    except OSError: pass
print(json.dumps({{'seen':seen,'signalled':signalled,'root':readable({root:?}),'mounted':readable({mounted:?}),'cwd':os.getcwd(),'fds':fds}}))
"#,
        root = sentinel.to_string_lossy(),
        mounted = mounted_sentinel.to_string_lossy(),
    );
    let second_launch = isolated_prepare(
        "python3",
        vec![OsString::from("-c"), OsString::from(probe)],
        None,
    )
    .unwrap();
    let output = std::process::Command::new(second_launch.program)
        .args(second_launch.args)
        .envs(second_launch.env)
        .env("SIBLING_SECRET", "second-only")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["seen"], serde_json::json!([]));
    assert_eq!(result["signalled"], false);
    assert_eq!(result["root"], false);
    assert_eq!(result["mounted"], false);
    assert_eq!(result["cwd"], "/state");
    let fds = result["fds"].as_array().unwrap();
    assert!(
        fds.iter().all(|value| {
            let value = value.as_str().unwrap_or_default();
            !value.contains("/mnt/")
                && !value.contains(sentinel_root.path().to_string_lossy().as_ref())
        }),
        "{fds:?}"
    );

    let _ = first.kill();
    let _ = first.wait();
    assert_eq!(unsafe { libc::umount2(mount_raw.as_ptr(), 0) }, 0);
}

#[test]
fn authorized_snapshot_script_still_executes() {
    if unsafe { libc::geteuid() } != 0 || !Path::new("/usr/bin/bwrap").exists() {
        return;
    }
    let _lock = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let script = source.path().join("tool.py");
    fs::write(
        &script,
        b"import os\nprint('snapshot-ok')\nprint(os.getcwd())\n",
    )
    .unwrap();
    let _enabled = crate::test_env::TestEnvVarGuard::set(ENABLE_ENV, "1");
    let _home = crate::test_env::TestEnvVarGuard::set("HOME", home.path());
    let _proc = crate::test_env::TestEnvVarGuard::remove("COS_PROC_DATA_DIR");
    let _broker = crate::test_env::TestEnvVarGuard::remove("COS_EXTENSION_BROKER_SOCKET");
    let launch = isolated_prepare(
        "python3",
        vec![script.as_os_str().to_os_string()],
        Some(source.path()),
    )
    .unwrap();
    let output = std::process::Command::new(launch.program)
        .args(launch.args)
        .envs(launch.env)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("snapshot-ok\n"));
    assert!(stdout.contains(source.path().to_string_lossy().as_ref()));
}

#[test]
fn verified_private_script_executes_with_pinned_interpreter() {
    if unsafe { libc::geteuid() } != 0 || !Path::new("/usr/bin/bwrap").exists() {
        return;
    }
    let _lock = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let script = source.path().join("tool");
    fs::write(&script, b"#!/usr/bin/python3\nprint('direct-ok')\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o500)).unwrap();
    let _enabled = crate::test_env::TestEnvVarGuard::set(ENABLE_ENV, "1");
    let _home = crate::test_env::TestEnvVarGuard::set("HOME", home.path());
    let _proc = crate::test_env::TestEnvVarGuard::remove("COS_PROC_DATA_DIR");
    let _broker = crate::test_env::TestEnvVarGuard::remove("COS_EXTENSION_BROKER_SOCKET");
    let launch =
        isolated_prepare(&script, Vec::<OsString>::new(), Some(source.path())).unwrap();
    let output = std::process::Command::new(launch.program)
        .env_clear()
        .args(launch.args)
        .envs(launch.env)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"direct-ok\n");
}

#[test]
fn authorized_sdk_snapshot_remains_importable() {
    if unsafe { libc::geteuid() } != 0 || !Path::new("/usr/bin/bwrap").exists() {
        return;
    }
    let _lock = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let sdk = tempfile::tempdir().unwrap();
    fs::write(sdk.path().join("isolated_sdk_probe.py"), b"VALUE='sdk-ok'\n").unwrap();
    let _enabled = crate::test_env::TestEnvVarGuard::set(ENABLE_ENV, "1");
    let _home = crate::test_env::TestEnvVarGuard::set("HOME", home.path());
    let _sdk = crate::test_env::TestEnvVarGuard::set("COS_SDK_PYTHON_DIR", sdk.path());
    let _proc = crate::test_env::TestEnvVarGuard::remove("COS_PROC_DATA_DIR");
    let _broker = crate::test_env::TestEnvVarGuard::remove("COS_EXTENSION_BROKER_SOCKET");
    let launch = isolated_prepare(
        "python3",
        vec![
            "-c".into(),
            "import isolated_sdk_probe; print(isolated_sdk_probe.VALUE)".into(),
        ],
        None,
    )
    .unwrap();
    let output = std::process::Command::new(launch.program)
        .env_clear()
        .args(launch.args)
        .envs(launch.env)
        .env("PYTHONPATH", sdk.path())
        .env("COS_SDK_PYTHON_DIR", sdk.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"sdk-ok\n");
}

#[test]
fn missing_custom_script_interpreter_fails_closed() {
    let _lock = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let script = source.path().join("tool");
    fs::write(&script, b"#!/missing/interpreter\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o500)).unwrap();
    let _enabled = crate::test_env::TestEnvVarGuard::set(ENABLE_ENV, "1");
    let _home = crate::test_env::TestEnvVarGuard::set("HOME", home.path());
    assert!(
        isolated_prepare(&script, Vec::<OsString>::new(), Some(source.path()))
            .unwrap_err()
            .contains("interpreter")
    );
}

#[test]
fn private_network_blocks_host_and_sibling_endpoints() {
    use std::net::{TcpListener, UdpSocket};
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener};

    if unsafe { libc::geteuid() } != 0 || !Path::new("/usr/bin/bwrap").exists() {
        return;
    }
    let _lock = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let ipv6 = TcpListener::bind("[::1]:0").ok();
    let ipv6_port = ipv6
        .as_ref()
        .map(|listener| listener.local_addr().unwrap().port())
        .unwrap_or(0);
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    udp.set_read_timeout(Some(std::time::Duration::from_millis(250)))
        .unwrap();
    let udp_port = udp.local_addr().unwrap().port();
    let abstract_name = format!("cos-isolation-{}", uuid::Uuid::new_v4().simple());
    let abstract_address = SocketAddr::from_abstract_name(abstract_name.as_bytes()).unwrap();
    let _abstract_listener = UnixListener::bind_addr(&abstract_address).unwrap();
    let unix_dir = tempfile::tempdir().unwrap();
    let unix_path = unix_dir.path().join("sibling.sock");
    let _unix_listener = UnixListener::bind(&unix_path).unwrap();
    let host_namespace = fs::read_link("/proc/self/ns/net").unwrap();
    let _enabled = crate::test_env::TestEnvVarGuard::set(ENABLE_ENV, "1");
    let _home = crate::test_env::TestEnvVarGuard::set("HOME", home.path());
    let _proc = crate::test_env::TestEnvVarGuard::remove("COS_PROC_DATA_DIR");
    let _broker = crate::test_env::TestEnvVarGuard::remove("COS_EXTENSION_BROKER_SOCKET");
    let script = format!(
        r#"import json,socket
def blocked(family, address):
    try:
        s=socket.socket(family, socket.SOCK_STREAM)
        s.settimeout(0.2)
        s.connect(address)
        return False
    except OSError:
        return True
def udp_blocked(address):
    try:
        s=socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(address)
        s.send(b"x")
        return False
    except OSError:
        return True
try:
    netlink=socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, 0)
    netlink.bind((0,0))
    netlink_pid=netlink.getsockname()[0]
except OSError:
    netlink_pid=-1
interfaces=[]
with open("/proc/net/dev") as f:
    for line in f.readlines()[2:]:
        interfaces.append(line.split(":",1)[0].strip())
print(json.dumps({{
  "loopback": blocked(socket.AF_INET, ("127.0.0.1", {port})),
  "internet": blocked(socket.AF_INET, ("1.1.1.1", 53)),
  "ipv6_loopback": blocked(socket.AF_INET6, ("::1", {ipv6_port})) if {ipv6_port} else True,
  "ipv6_internet": blocked(socket.AF_INET6, ("2606:4700:4700::1111", 53)),
  "udp": udp_blocked(("127.0.0.1", {udp_port})),
  "unix": blocked(socket.AF_UNIX, {unix_path:?}),
  "abstract": blocked(socket.AF_UNIX, "\0{abstract_name}"),
  "netlink_pid": netlink_pid,
  "interfaces": interfaces,
  "namespace": __import__("os").readlink("/proc/self/ns/net"),
}}))
"#
    );
    let launch = isolated_prepare("python3", vec!["-c".into(), script.into()], None).unwrap();
    let output = std::process::Command::new(launch.program)
        .env_clear()
        .args(launch.args)
        .envs(launch.env)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["loopback"], true);
    assert_eq!(result["internet"], true);
    assert_eq!(result["ipv6_loopback"], true);
    assert_eq!(result["ipv6_internet"], true);
    assert_eq!(result["unix"], true);
    assert_eq!(result["abstract"], true);
    assert!(result["netlink_pid"].as_i64().unwrap_or(-1) >= 0);
    assert_eq!(result["interfaces"], serde_json::json!(["lo"]));
    assert_ne!(
        result["namespace"].as_str().unwrap(),
        host_namespace.to_string_lossy()
    );
    let mut datagram = [0u8; 1];
    assert!(udp.recv(&mut datagram).is_err());
}

#[test]
fn inherited_connected_socket_is_closed_before_bwrap_exec() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::os::fd::AsRawFd;
    use std::time::Duration;

    if unsafe { libc::geteuid() } != 0 || !Path::new("/usr/bin/bwrap").exists() {
        return;
    }
    let _lock = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut inherited = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut peer, _) = listener.accept().unwrap();
    inherited.set_nonblocking(false).unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(250))).unwrap();
    let fd = inherited.as_raw_fd();
    let listener_fd = listener.as_raw_fd();
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
    assert_eq!(unsafe { libc::fcntl(listener_fd, libc::F_SETFD, 0) }, 0);
    let _enabled = crate::test_env::TestEnvVarGuard::set(ENABLE_ENV, "1");
    let _home = crate::test_env::TestEnvVarGuard::set("HOME", home.path());
    let _proc = crate::test_env::TestEnvVarGuard::remove("COS_PROC_DATA_DIR");
    let _broker = crate::test_env::TestEnvVarGuard::remove("COS_EXTENSION_BROKER_SOCKET");
    let script = format!(
        "import json,os\nresult=[]\nfor fd in [{fd},{listener_fd}]:\n try:\n  os.fstat(fd); result.append('open')\n except OSError:\n  result.append('closed')\nprint(json.dumps(result))"
    );
    let launch = isolated_prepare("python3", vec!["-c".into(), script.into()], None).unwrap();
    let mut command = std::process::Command::new(launch.program);
    close_unallowlisted_fds(&mut command);
    let output = command
        .env_clear()
        .args(launch.args)
        .envs(launch.env)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!(["closed", "closed"])
    );
    let mut byte = [0u8; 1];
    assert!(peer.read(&mut byte).is_err());
    inherited.write_all(b"x").unwrap();
}

#[test]
fn isolated_siblings_cannot_reach_each_others_network_endpoints() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    if unsafe { libc::geteuid() } != 0 || !Path::new("/usr/bin/bwrap").exists() {
        return;
    }
    let _lock = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let abstract_name = format!("cos-sibling-{}", uuid::Uuid::new_v4().simple());
    let _enabled = crate::test_env::TestEnvVarGuard::set(ENABLE_ENV, "1");
    let _home = crate::test_env::TestEnvVarGuard::set("HOME", home.path());
    let _proc = crate::test_env::TestEnvVarGuard::remove("COS_PROC_DATA_DIR");
    let _broker = crate::test_env::TestEnvVarGuard::remove("COS_EXTENSION_BROKER_SOCKET");
    let first_script = format!(
        r#"import json,socket,time
t=socket.socket(); t.bind(("0.0.0.0",0)); t.listen()
u=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); u.bind(("0.0.0.0",0))
a=socket.socket(socket.AF_UNIX); a.bind("\0{abstract_name}"); a.listen()
print(json.dumps({{"tcp":t.getsockname()[1],"udp":u.getsockname()[1]}}),flush=True)
u.settimeout(3)
try:
 u.recv(1); received=True
except OSError:
 received=False
print(json.dumps({{"udp_received":received}}),flush=True)
time.sleep(30)
"#
    );
    let first_launch =
        isolated_prepare("python3", vec!["-c".into(), first_script.into()], None).unwrap();
    let mut first_command = std::process::Command::new(first_launch.program);
    close_unallowlisted_fds(&mut first_command);
    let mut first = first_command
        .env_clear()
        .args(first_launch.args)
        .envs(first_launch.env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first_stdout = BufReader::new(first.stdout.take().unwrap());
    let mut line = String::new();
    first_stdout.read_line(&mut line).unwrap();
    let endpoints: serde_json::Value = serde_json::from_str(&line).unwrap();
    let second_script = format!(
        r#"import json,socket
def tcp():
 try:
  s=socket.socket(); s.settimeout(.2); s.connect(("127.0.0.1",{tcp})); return False
 except OSError: return True
def udp():
 try:
  s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.connect(("127.0.0.1",{udp})); s.send(b"x"); return False
 except OSError: return True
def abstract():
 try:
  s=socket.socket(socket.AF_UNIX); s.connect("\0{abstract_name}"); return False
 except OSError: return True
print(json.dumps({{"tcp":tcp(),"udp":udp(),"abstract":abstract()}}))
"#,
        tcp = endpoints["tcp"].as_u64().unwrap(),
        udp = endpoints["udp"].as_u64().unwrap(),
    );
    let second_launch =
        isolated_prepare("python3", vec!["-c".into(), second_script.into()], None).unwrap();
    let mut second_command = std::process::Command::new(second_launch.program);
    close_unallowlisted_fds(&mut second_command);
    let output = second_command
        .env_clear()
        .args(second_launch.args)
        .envs(second_launch.env)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let result = serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
    assert_eq!(result["tcp"], true);
    assert_eq!(result["abstract"], true);
    let mut udp_result = String::new();
    first_stdout.read_line(&mut udp_result).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&udp_result).unwrap()["udp_received"],
        false
    );
    let _ = first.kill();
    let _ = first.wait();
}

#[test]
fn runtime_snapshot_rejects_reserved_owners_writable_entries_and_mounts() {
    use std::os::unix::ffi::OsStrExt;

    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let source = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let authority = test_authority(&[source.path()]);
    let unsafe_file = source.path().join("unsafe");
    fs::write(&unsafe_file, b"x").unwrap();
    let raw = std::ffi::CString::new(unsafe_file.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe { libc::chown(raw.as_ptr(), EXTENSION_UID_START, 0) },
        0
    );
    let mut args = Vec::new();
    assert!(snapshot_runtime_tree(
        source.path(),
        &output.path().join("reserved"),
        Path::new("/usr/share/test-runtime"),
        &mut args,
        &authority,
    )
    .unwrap_err()
    .contains("unsafe ownership"));

    assert_eq!(unsafe { libc::chown(raw.as_ptr(), 0, 0) }, 0);
    fs::set_permissions(&unsafe_file, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(snapshot_runtime_tree(
        source.path(),
        &output.path().join("writable"),
        Path::new("/usr/share/test-runtime"),
        &mut Vec::new(),
        &authority,
    )
    .unwrap_err()
    .contains("unsafe ownership"));

    fs::set_permissions(&unsafe_file, fs::Permissions::from_mode(0o400)).unwrap();
    let mountpoint = source.path().join("mounted");
    fs::create_dir(&mountpoint).unwrap();
    let mount_raw = std::ffi::CString::new(mountpoint.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::mount(
                c"tmpfs".as_ptr(),
                mount_raw.as_ptr(),
                c"tmpfs".as_ptr(),
                0,
                c"size=4096".as_ptr().cast(),
            )
        },
        0
    );
    let error = snapshot_runtime_tree(
        source.path(),
        &output.path().join("mount"),
        Path::new("/usr/share/test-runtime"),
        &mut Vec::new(),
        &authority,
    )
    .unwrap_err();
    assert!(error.contains("crosses a mount"), "{error}");
    assert_eq!(unsafe { libc::umount2(mount_raw.as_ptr(), 0) }, 0);

    std::os::unix::fs::symlink("/etc/passwd", source.path().join("escape")).unwrap();
    fs::hard_link(&unsafe_file, source.path().join("hardlink")).unwrap();
    let filtered = output.path().join("filtered");
    snapshot_runtime_tree(
        source.path(),
        &filtered,
        Path::new("/usr/share/test-runtime"),
        &mut Vec::new(),
        &authority,
    )
    .unwrap();
    assert!(!filtered.join("escape").exists());
    assert_ne!(
        fs::metadata(&unsafe_file).unwrap().ino(),
        fs::metadata(filtered.join("hardlink")).unwrap().ino()
    );

    assert!(snapshot_runtime_tree(
        Path::new("/usr/local"),
        &output.path().join("local"),
        Path::new("/usr/local"),
        &mut Vec::new(),
        &authority,
    )
    .unwrap_err()
    .contains("forbidden"));
}

#[test]
fn exact_broker_socket_remains_reachable_inside_the_empty_root() {
    use std::os::unix::net::UnixListener;

    if unsafe { libc::geteuid() } != 0 || !Path::new("/usr/bin/bwrap").exists() {
        return;
    }
    let _lock = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let socket = home.path().join("broker.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let _enabled = crate::test_env::TestEnvVarGuard::set(ENABLE_ENV, "1");
    let _home = crate::test_env::TestEnvVarGuard::set("HOME", home.path());
    let _proc = crate::test_env::TestEnvVarGuard::remove("COS_PROC_DATA_DIR");
    let _broker = crate::test_env::TestEnvVarGuard::set("COS_EXTENSION_BROKER_SOCKET", &socket);
    let script = "import os,socket; s=socket.socket(socket.AF_UNIX); s.connect(os.environ['COS_EXTENSION_BROKER_SOCKET']); s.send(b'ok')";
    let launch = isolated_prepare(
        "python3",
        vec![OsString::from("-c"), OsString::from(script)],
        None,
    )
    .unwrap();
    let child = std::process::Command::new(launch.program)
        .args(launch.args)
        .envs(launch.env)
        .spawn()
        .unwrap();
    let (mut accepted, _) = listener.accept().unwrap();
    let mut message = [0u8; 2];
    use std::io::Read;
    accepted.read_exact(&mut message).unwrap();
    assert_eq!(&message, b"ok");
    assert!(child.wait_with_output().unwrap().status.success());
}
