use super::*;
use std::io::{BufRead, BufReader};
use std::os::fd::AsRawFd;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Bus {
    child: Child,
    address: String,
    _directory: tempfile::TempDir,
}

impl Bus {
    fn new() -> Self {
        Self::start(tempfile::tempdir().unwrap(), None)
    }

    fn start(directory: tempfile::TempDir, owner: Option<u32>) -> Self {
        use std::os::unix::process::CommandExt;
        let address = format!("unix:path={}/bus", directory.path().display());
        let mut command = std::process::Command::new("dbus-daemon");
        command.args(["--nofork", "--print-address=1"]);
        if let Some(uid) = owner {
            std::os::unix::fs::chown(directory.path(), Some(uid), Some(uid)).unwrap();
            let config = directory.path().join("bus.conf");
            fs::write(
                &config,
                format!(
                "<busconfig><type>session</type><auth>EXTERNAL</auth><listen>{address}</listen>\
                <policy context=\"default\"><allow user=\"0\"/><allow user=\"{uid}\"/>\
                <allow own=\"*\"/><allow send_destination=\"*\"/>\
                <allow receive_sender=\"*\"/></policy></busconfig>"
            ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&config, fs::Permissions::from_mode(0o644)).unwrap();
            command
                .arg(format!("--config-file={}", config.display()))
                .uid(uid)
                .gid(uid);
        } else {
            command.arg("--session").arg(format!("--address={address}"));
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(address.starts_with("unix:"));
        Self {
            child,
            address: address.trim().into(),
            _directory: directory,
        }
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Portal {
    uri: String,
    status: u32,
    modal: bool,
    wrong_handle: bool,
    calls: Arc<AtomicUsize>,
}

#[zbus::interface(name = "org.freedesktop.portal.Screenshot")]
impl Portal {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }

    async fn screenshot(
        &self,
        parent_window: &str,
        mut options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(parent_window.is_empty());
        assert!(!bool::try_from(options.remove("interactive").unwrap()).unwrap());
        assert_eq!(
            bool::try_from(options.remove("modal").unwrap()).unwrap(),
            self.modal
        );
        let token = String::try_from(options.remove("handle_token").unwrap()).unwrap();
        let destination = header.sender().unwrap().to_string();
        let sender = destination.trim_start_matches(':').replace('.', "_");
        let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
        let values = HashMap::from([("uri", Value::from(self.uri.as_str()))]);
        connection
            .emit_signal(
                Some(destination.as_str()),
                path.as_str(),
                "org.freedesktop.portal.Request",
                "Response",
                &(self.status, values),
            )
            .await?;
        OwnedObjectPath::try_from(if self.wrong_handle {
            format!("{path}_other")
        } else {
            path
        })
        .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))
    }
}

#[tokio::test]
async fn portal_handles_early_response_cancellation_and_unrelated_handles() {
    for (status, wrong_handle, modal) in [
        (0, false, false),
        (1, false, true),
        (2, false, false),
        (0, true, true),
    ] {
        let bus = Bus::new();
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("capture with spaces.png");
        fs::write(&path, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let _service = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name("org.freedesktop.portal.Desktop")
            .unwrap()
            .serve_at(
                "/org/freedesktop/portal/desktop",
                Portal {
                    uri: url::Url::from_file_path(&path).unwrap().to_string(),
                    status,
                    modal,
                    wrong_handle,
                    calls: calls.clone(),
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            portal_path(&client, modal),
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        if wrong_handle {
            assert!(result.unwrap_err().contains("unrelated request handle"));
        } else if status == 0 {
            assert_eq!(result.unwrap(), Some(path.clone()));
            assert_eq!(consume_png(&path).unwrap(), b"\x89PNG\r\n\x1a\nfixture");
            assert!(!path.exists());
        } else if status == 1 {
            assert_eq!(result.unwrap(), None);
            assert!(path.exists());
        } else {
            assert!(result.unwrap_err().contains("refused"));
            assert!(path.exists());
        }
    }
}

#[test]
fn portal_image_refuses_links_invalid_bytes_and_oversized_files() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source.png");
    let link = temporary.path().join("link.png");
    fs::write(&source, b"\x89PNG\r\n\x1a\nfixture").unwrap();
    std::os::unix::fs::symlink(&source, &link).unwrap();
    assert!(consume_png(&link).is_err());
    fs::remove_file(&link).unwrap();
    fs::hard_link(&source, &link).unwrap();
    assert!(consume_png(&source).is_err());
    assert!(source.exists() && link.exists());
    fs::remove_file(&link).unwrap();
    fs::write(&source, b"not a PNG").unwrap();
    assert!(consume_png(&source).is_err());
    assert!(source.exists());
    let file = OpenOptions::new().write(true).open(&source).unwrap();
    file.set_len(MAX_PNG_BYTES as u64 + 1).unwrap();
    assert!(consume_png(&source).is_err());
    assert!(source.exists());
}

#[test]
fn portal_helper_refuses_self_issued_or_missing_broker_descriptors() {
    let (_parent, child) = UnixStream::pair().unwrap();
    assert!(helper(&["false".into(), child.as_raw_fd().to_string()])
        .unwrap_err()
        .contains("root clawd"));
    assert!(helper(&["false".into(), i32::MAX.to_string()])
        .unwrap_err()
        .contains("root clawd"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Root private mount/PID/network namespaces and the matching real clawd binary"]
async fn private_os_portal_helper_uses_root_dispatch_without_app_program() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    assert_eq!(unsafe { libc::geteuid() }, 0);
    for (variable, namespace) in [
        ("COS_CAPTURE_PARENT_MNT", "mnt"),
        ("COS_CAPTURE_PARENT_PID", "pid"),
        ("COS_CAPTURE_PARENT_NET", "net"),
    ] {
        assert_ne!(
            fs::read_link(format!("/proc/self/ns/{namespace}")).unwrap(),
            PathBuf::from(std::env::var(variable).unwrap()),
        );
    }
    let root = Path::new("/run/claw-capture-fixture");
    fs::create_dir(root).unwrap();
    fs::set_permissions(root, fs::Permissions::from_mode(0o755)).unwrap();
    let binary = root.join("clawd");
    fs::copy(std::env::var_os("COS_CAPTURE_TEST_CLAWD").unwrap(), &binary).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    const OWNER: u32 = 65534;
    for (status, dispatch) in [(0, 0), (0, 1), (1, 1), (2, 1)] {
        let bus = Bus::start(tempfile::tempdir_in(root).unwrap(), Some(OWNER));
        let data = tempfile::tempdir_in(root).unwrap();
        let path = data.path().join("capture.png");
        fs::write(&path, b"\x89PNG\r\n\x1a\nactual-private-helper").unwrap();
        for path in [data.path(), path.as_path()] {
            std::os::unix::fs::chown(path, Some(OWNER), Some(OWNER)).unwrap();
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let _service = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name("org.freedesktop.portal.Desktop")
            .unwrap()
            .serve_at(
                "/org/freedesktop/portal/desktop",
                Portal {
                    uri: url::Url::from_file_path(&path).unwrap().to_string(),
                    status,
                    modal: false,
                    wrong_handle: false,
                    calls: calls.clone(),
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        let (mut gate, peer) = UnixStream::pair().unwrap();
        let fd = peer.as_raw_fd();
        let mut command = std::process::Command::new(&binary);
        command
            .args(super::super::helper_args(false, fd))
            .env_clear()
            .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
            .current_dir("/");
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0
                    || libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(OWNER) != 0
                    || libc::setuid(OWNER) != 0
                    || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        gate.write_all(&[dispatch]).unwrap();
        let result = super::super::run_capture(command, std::time::Duration::from_secs(5)).await;
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(dispatch == 1));
        if dispatch == 0 {
            assert!(result.unwrap_err().contains("not authorized"));
            assert!(path.exists());
        } else if status == 0 {
            assert_eq!(result.unwrap(), b"\x89PNG\r\n\x1a\nactual-private-helper");
            assert!(!path.exists());
        } else if status == 1 {
            assert!(result.unwrap().is_empty());
            assert!(path.exists());
        } else {
            assert!(result.unwrap_err().contains("refused"));
            assert!(path.exists());
        }
    }
}
