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
        "--proc /proc",
        "--tmpfs /",
        "--ro-bind /usr /usr",
    ] {
        assert!(rendered.contains(required), "{rendered}");
    }
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
    fs::write(&script, b"print('snapshot-ok')\n").unwrap();
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
    assert_eq!(output.stdout, b"snapshot-ok\n");
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
