use super::super::tests::Bus;
use super::*;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Portal {
    uri: String,
    status: u32,
    modal: bool,
    wait: bool,
    defer_reply: bool,
    acknowledge: bool,
    wrong_handle: bool,
    calls: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
}

struct Pending {
    acknowledge: bool,
    closes: Arc<AtomicUsize>,
    closed: Arc<tokio::sync::Notify>,
}

#[zbus::interface(name = "org.freedesktop.portal.Request")]
impl Pending {
    fn close(&self) -> zbus::fdo::Result<()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        self.closed.notify_one();
        if self.acknowledge {
            Ok(())
        } else {
            Err(zbus::fdo::Error::Failed("Close unavailable".into()))
        }
    }
}

#[zbus::interface(name = "org.freedesktop.portal.Screenshot")]
impl Portal {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }

    async fn screenshot(
        &self,
        parent: &str,
        mut options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(parent.is_empty());
        assert!(bool::try_from(options.remove("interactive").unwrap()).unwrap());
        assert_eq!(
            bool::try_from(options.remove("modal").unwrap()).unwrap(),
            self.modal
        );
        let token = String::try_from(options.remove("handle_token").unwrap()).unwrap();
        let destination = header.sender().unwrap().to_string();
        let sender = destination.trim_start_matches(':').replace('.', "_");
        let path = OwnedObjectPath::try_from(format!(
            "/org/freedesktop/portal/desktop/request/{sender}/{token}",
        ))
        .unwrap();
        let closed = Arc::new(tokio::sync::Notify::new());
        connection
            .object_server()
            .at(
                path.clone(),
                Pending {
                    acknowledge: self.acknowledge,
                    closes: self.closes.clone(),
                    closed: closed.clone(),
                },
            )
            .await
            .unwrap();
        if !self.wait {
            connection
                .emit_signal(
                    Some(destination.as_str()),
                    path.clone(),
                    REQUEST_INTERFACE,
                    "Response",
                    &(
                        self.status,
                        HashMap::from([("uri", Value::from(self.uri.as_str()))]),
                    ),
                )
                .await?;
        }
        if self.defer_reply {
            closed.notified().await;
        }
        Ok(if self.wrong_handle {
            OwnedObjectPath::try_from(format!("{path}_unrelated")).unwrap()
        } else {
            path
        })
    }
}

fn portal(uri: &str, status: u32) -> Portal {
    Portal {
        uri: uri.into(),
        status,
        modal: false,
        wait: false,
        defer_reply: false,
        acknowledge: true,
        wrong_handle: false,
        calls: Arc::new(AtomicUsize::new(0)),
        closes: Arc::new(AtomicUsize::new(0)),
    }
}

async fn service(bus: &Bus, portal: Portal) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(DESTINATION)
        .unwrap()
        .serve_at("/org/freedesktop/portal/desktop", portal)
        .unwrap()
        .build()
        .await
        .unwrap()
}

async fn channel() -> (tokio::net::UnixStream, tokio::net::unix::OwnedReadHalf) {
    let (parent, child) = tokio::net::UnixStream::pair().unwrap();
    (parent, child.into_split().0)
}

#[tokio::test]
async fn interactive_portal_preserves_early_file_clipboard_and_cancel_results_as_metadata() {
    let _lock = crate::test_env::lock_env();
    for (uri, status, expected) in [
        (
            "file:///home/fixture/Pictures/human%20choice.png",
            0,
            "saved",
        ),
        ("clipboard:", 0, "clipboard"),
        ("file:///unused", 1, "cancelled"),
        ("https://example.invalid/image", 0, "failed"),
    ] {
        let bus = Bus::new();
        let _environment =
            crate::test_env::TestEnvVarGuard::set("DBUS_SESSION_BUS_ADDRESS", &bus.address);
        let _service = service(&bus, portal(uri, status)).await;
        let (_parent, mut control) = channel().await;
        let report = tokio::time::timeout(Duration::from_secs(3), interact(false, &mut control))
            .await
            .unwrap();
        assert!(report.portal_retired);
        let value = serde_json::to_value(report.outcome).unwrap();
        assert_eq!(value["status"], expected);
        if expected == "saved" {
            assert_eq!(value["path"], "/home/fixture/Pictures/human choice.png");
        } else {
            assert!(value.get("path").is_none());
        }
    }
}

#[tokio::test]
async fn interactive_portal_waits_for_human_but_cancel_requires_real_close_acknowledgement() {
    let _lock = crate::test_env::lock_env();
    for acknowledge in [true, false] {
        let bus = Bus::new();
        let _environment =
            crate::test_env::TestEnvVarGuard::set("DBUS_SESSION_BUS_ADDRESS", &bus.address);
        let mut fixture = portal("file:///unused", 0);
        fixture.wait = true;
        fixture.defer_reply = true;
        fixture.acknowledge = acknowledge;
        let calls = fixture.calls.clone();
        let closes = fixture.closes.clone();
        let _service = service(&bus, fixture).await;
        let (mut parent, mut control) = channel().await;
        let capture = interact(false, &mut control);
        tokio::pin!(capture);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut capture)
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(21)).await;
        assert!(tokio::time::timeout(Duration::from_millis(1), &mut capture)
            .await
            .is_err());
        tokio::time::resume();
        let _replacement = if acknowledge {
            _service.release_name(DESTINATION).await.unwrap();
            Some(service(&bus, portal("clipboard:", 0)).await)
        } else {
            None
        };
        parent.write_u8(2).await.unwrap();
        let report = tokio::time::timeout(Duration::from_secs(3), &mut capture)
            .await
            .unwrap();
        assert_eq!(report.portal_retired, acknowledge);
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert!(matches!(report.outcome, Outcome::Cancelled) == acknowledge);
    }
    for uri in [
        "file://other-host/image.png",
        "file:///image%00.png",
        "file:///a%0Ab",
        "not a URI",
    ] {
        assert!(destination(uri).is_err());
    }
}

#[tokio::test]
async fn interactive_portal_cannot_adopt_an_unrelated_handle() {
    let _lock = crate::test_env::lock_env();
    let bus = Bus::new();
    let _environment =
        crate::test_env::TestEnvVarGuard::set("DBUS_SESSION_BUS_ADDRESS", &bus.address);
    let mut fixture = portal("clipboard:", 0);
    fixture.wrong_handle = true;
    let closes = fixture.closes.clone();
    let _service = service(&bus, fixture).await;
    let (_parent, mut control) = channel().await;
    let report = interact(false, &mut control).await;
    assert!(
        !report.portal_retired,
        "closing the expected token cannot retire an unrelated returned handle"
    );
    assert!(matches!(report.outcome, Outcome::Failed { .. }));
    assert_eq!(closes.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Root private mount/PID/network/run namespaces and the matching real clawd binary"]
async fn private_interactive_helper_reports_only_after_os_portal_close() {
    use std::fs;
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
            std::path::PathBuf::from(std::env::var(variable).unwrap())
        );
    }
    let root = std::path::Path::new("/run/claw-capture-gui-fixture");
    fs::create_dir(root).unwrap();
    fs::set_permissions(root, fs::Permissions::from_mode(0o755)).unwrap();
    let binary = root.join("clawd");
    fs::copy(std::env::var_os("COS_CAPTURE_TEST_CLAWD").unwrap(), &binary).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    const OWNER: u32 = 65534;
    for (wait, acknowledge, defer_reply) in [
        (false, true, false),
        (true, true, false),
        (true, true, true),
        (true, false, true),
    ] {
        let bus = Bus::start(tempfile::tempdir_in(root).unwrap(), Some(OWNER));
        let mut fixture = portal("clipboard:", 0);
        fixture.wait = wait;
        fixture.acknowledge = acknowledge;
        fixture.defer_reply = defer_reply;
        let calls = fixture.calls.clone();
        let closes = fixture.closes.clone();
        let _service = service(&bus, fixture).await;
        let (mut parent, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let fd = peer.as_raw_fd();
        let mut command = std::process::Command::new(&binary);
        command
            .args(["--capture-gui-helper", "false", &fd.to_string()])
            .env_clear()
            .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
            .current_dir("/")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
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
        let mut child = tokio::process::Command::from(command).spawn().unwrap();
        drop(peer);
        parent.write_all(&[1]).unwrap();
        parent.set_nonblocking(true).unwrap();
        let mut parent = tokio::net::UnixStream::from_std(parent).unwrap();
        if wait {
            tokio::time::timeout(Duration::from_secs(3), async {
                while calls.load(Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            parent.write_u8(2).await.unwrap();
        }
        let length = tokio::time::timeout(Duration::from_secs(4), parent.read_u32())
            .await
            .unwrap()
            .unwrap() as usize;
        assert!(length > 0 && length <= REPORT_BYTES);
        let mut bytes = vec![0; length];
        parent.read_exact(&mut bytes).await.unwrap();
        let report: Report = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(report.portal_retired, !wait || acknowledge);
        assert!(tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success());
        assert_eq!(closes.load(Ordering::SeqCst), usize::from(wait));
        assert!(matches!(report.outcome, Outcome::Clipboard) == !wait);
    }
}
