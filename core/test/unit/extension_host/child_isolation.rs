use super::*;

#[test]
fn disabled_child_isolation_preserves_the_original_command() {
    let _lock = crate::test_env::lock_env();
    let _guard = crate::test_env::TestEnvVarGuard::remove(ENABLE_ENV);
    let launch = prepare("python3", vec![OsString::from("-V")], None).unwrap();
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
        let error = prepare_with_clean_env(
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
    let launch = prepare_with_clean_env(
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
    let launch = prepare(
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
        prepare("python3", Vec::<OsString>::new(), Some(source.path()))
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
    let first_launch = prepare(
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
    let second_launch = prepare(
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
    let launch = prepare(
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
    let launch = prepare(&script, Vec::<OsString>::new(), Some(source.path())).unwrap();
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
    let launch = prepare(
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
        prepare(&script, Vec::<OsString>::new(), Some(source.path()))
            .unwrap_err()
            .contains("interpreter")
    );
}

#[test]
fn private_network_blocks_host_loopback() {
    use std::net::TcpListener;
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener};

    if unsafe { libc::geteuid() } != 0 || !Path::new("/usr/bin/bwrap").exists() {
        return;
    }
    let _lock = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let abstract_name = format!("cos-isolation-{}", uuid::Uuid::new_v4().simple());
    let abstract_address = SocketAddr::from_abstract_name(abstract_name.as_bytes()).unwrap();
    let _abstract_listener = UnixListener::bind_addr(&abstract_address).unwrap();
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
print(json.dumps({{
  "loopback": blocked(socket.AF_INET, ("127.0.0.1", {port})),
  "internet": blocked(socket.AF_INET, ("1.1.1.1", 53)),
  "abstract": blocked(socket.AF_UNIX, "\0{abstract_name}"),
}}))
"#
    );
    let launch = prepare("python3", vec!["-c".into(), script.into()], None).unwrap();
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
    assert_eq!(result["abstract"], true);
}

#[test]
fn runtime_snapshot_rejects_reserved_owners_writable_entries_and_mounts() {
    use std::os::unix::ffi::OsStrExt;

    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let source = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
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
    let launch = prepare(
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
