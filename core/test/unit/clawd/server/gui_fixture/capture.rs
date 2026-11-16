use super::*;
use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncBufReadExt;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::Connection;

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const SAVED: &str = "/run/display-home/native capture saved.png";
const CONTENT: &[u8] = b"private synthetic portal-owned bytes; not a physical screenshot";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nactual-private-portal-output";
const IMAGE: &str = "/run/display-home/native portal temporary.png";

struct Call {
    sender: String,
    path: OwnedObjectPath,
    process: ProcessIdentity,
    pidfd: OwnedFd,
}

struct Control {
    case: usize,
    calls: AtomicUsize,
    closes: AtomicUsize,
    acknowledged: AtomicUsize,
    close_release: tokio::sync::Notify,
    call: Mutex<Option<Call>>,
}

struct Portal(Arc<Control>);
struct Pending(Arc<Control>);

#[zbus::interface(name = "org.freedesktop.portal.Request")]
impl Pending {
    async fn close(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        assert_eq!(
            header.sender().unwrap().as_str(),
            self.0.call.lock().unwrap().as_ref().unwrap().sender,
        );
        assert_eq!(self.0.closes.fetch_add(1, Ordering::SeqCst), 0);
        self.0.close_release.notified().await;
        self.0.acknowledged.store(1, Ordering::SeqCst);
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
        assert!(parent.is_empty());
        assert_eq!(
            bool::try_from(options.remove("interactive").unwrap()).unwrap(),
            self.0.case != 39
        );
        assert_eq!(
            bool::try_from(options.remove("modal").unwrap()).unwrap(),
            !matches!(self.0.case, 35 | 37)
        );
        let token = String::try_from(options.remove("handle_token").unwrap()).unwrap();
        assert!(options.is_empty());
        assert!(token
            .strip_prefix("claw_portal_")
            .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())));
        let sender = header.sender().unwrap().to_string();
        let bus = zbus::fdo::DBusProxy::new(connection).await.unwrap();
        let name = sender.as_str().try_into().unwrap();
        let pid = bus.get_connection_unix_process_id(name).await.unwrap();
        assert_eq!(
            bus.get_connection_unix_user(sender.as_str().try_into().unwrap())
                .await
                .unwrap(),
            OWNER
        );
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
        assert!(raw >= 0);
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        let process = ProcessIdentity::from_pidfd(pidfd.as_fd()).unwrap();
        assert_eq!(process.uid(), OWNER);
        let expected = std::fs::metadata("/usr/local/bin/clawd").unwrap();
        let actual = std::fs::metadata(format!("/proc/{pid}/exe")).unwrap();
        assert_eq!((actual.dev(), actual.ino()), (expected.dev(), expected.ino()),
            "the portal must be contacted by the real installed OS helper, never the App or a fixture helper");
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        assert!(status
            .lines()
            .any(|line| line.split_whitespace().eq(["NoNewPrivs:", "1"])));
        assert!(status.lines().any(|line| line
            .strip_prefix("PPid:")
            .is_some_and(|value| value.trim() == std::process::id().to_string())));
        let unique = sender.trim_start_matches(':').replace('.', "_");
        let path = OwnedObjectPath::try_from(format!(
            "/org/freedesktop/portal/desktop/request/{unique}/{token}",
        ))
        .unwrap();
        connection
            .object_server()
            .at(path.clone(), Pending(self.0.clone()))
            .await
            .unwrap();
        assert_eq!(self.0.calls.fetch_add(1, Ordering::SeqCst), 0);
        *self.0.call.lock().unwrap() = Some(Call {
            sender,
            path: path.clone(),
            process,
            pidfd,
        });
        Ok(path)
    }
}

struct Fixture {
    bus: tokio::process::Child,
    connection: Connection,
    control: Arc<Control>,
}

impl Fixture {
    async fn start(case: usize) -> Self {
        let config = Path::new("/run/gui-public/capture-bus.conf");
        if !config.exists() {
            std::fs::write(config, format!(
                "<busconfig><type>session</type><auth>EXTERNAL</auth>\
                <listen>unix:path=/run/user/{OWNER}/bus</listen>\
                <policy context=\"default\"><allow user=\"0\"/><allow user=\"{OWNER}\"/>\
                <deny own=\"*\"/><allow send_destination=\"*\"/><allow receive_sender=\"*\"/></policy>\
                <policy user=\"root\"><allow own=\"{DESTINATION}\"/></policy></busconfig>"
            )).unwrap();
            std::fs::set_permissions(config, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert!(!Path::new("/run/user/62050/bus").exists());
        let mut command = ProcessCommand::new("/usr/bin/dbus-daemon");
        command
            .args([
                "--nofork",
                "--nopidfile",
                "--print-address=1",
                "--config-file=/run/gui-public/capture-bus.conf",
            ])
            .env_clear()
            .env("HOME", "/run/display-home")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        unsafe {
            command.pre_exec(|| {
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(OWNER) != 0
                    || libc::setuid(OWNER) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let mut bus = command.spawn().unwrap();
        let mut address = String::new();
        tokio::time::timeout(
            Duration::from_secs(3),
            tokio::io::BufReader::new(bus.stdout.take().unwrap()).read_line(&mut address),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(address.starts_with("unix:path=/run/user/62050/bus"));
        let control = Arc::new(Control {
            case,
            calls: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            acknowledged: AtomicUsize::new(0),
            close_release: tokio::sync::Notify::new(),
            call: Mutex::new(None),
        });
        let connection = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .name(DESTINATION)
            .unwrap()
            .serve_at("/org/freedesktop/portal/desktop", Portal(control.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        Self {
            bus,
            connection,
            control,
        }
    }

    async fn response(&self, code: u32, uri: &str, unrelated: bool) {
        let (sender, path) = {
            let call = self.control.call.lock().unwrap();
            let call = call.as_ref().unwrap();
            (
                call.sender.clone(),
                if unrelated {
                    OwnedObjectPath::try_from(format!("{}_unrelated", call.path)).unwrap()
                } else {
                    call.path.clone()
                },
            )
        };
        self.connection
            .emit_signal(
                Some(sender.as_str()),
                path,
                "org.freedesktop.portal.Request",
                "Response",
                &(code, HashMap::from([("uri", Value::from(uri))])),
            )
            .await
            .unwrap();
    }

    fn helper_exited(&self) -> bool {
        let call = self.control.call.lock().unwrap();
        claw_display_control::identity::pidfd_exited(call.as_ref().unwrap().pidfd.as_fd()).unwrap()
    }

    async fn wait_close(&self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.control.closes.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "{}",
                diagnostic("Capture portal Close")
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            !self.helper_exited(),
            "a kill is not portal retirement evidence"
        );
    }

    async fn stop(mut self) {
        self.connection.close().await.unwrap();
        self.bus.start_kill().unwrap();
        tokio::time::timeout(Duration::from_secs(3), self.bus.wait())
            .await
            .unwrap()
            .unwrap();
        std::fs::remove_file("/run/user/62050/bus").unwrap_or_else(|error| {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        });
    }
}

fn completed(session: &str) -> serde_json::Value {
    let file = std::fs::File::open(crate::paths::provenance_audit_path()).unwrap();
    assert!(file.metadata().unwrap().len() <= 2 * 1024 * 1024);
    let mut bytes = String::new();
    file.take(2 * 1024 * 1024 + 1)
        .read_to_string(&mut bytes)
        .unwrap();
    let entries = bytes
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|record| {
            record["kind"] == "gui.capture.finished" && record["session_id"] == session
        })
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    let event = entries.into_iter().next().unwrap();
    assert_eq!(event["app_id"], CAPTURE.app);
    assert_eq!(event["helper_exited"], true);
    assert_eq!(event["helper_success"], true);
    assert_eq!(event["portal_retired"], true);
    assert_eq!(event["retired"], true);
    assert!(event["request_id"]
        .as_str()
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())));
    event
}

fn notifications() -> Vec<crate::notifications::Notification> {
    use crate::notifications::NotificationService;
    let service = crate::notifications::open_default().unwrap();
    assert_eq!(
        service
            .list_source(OWNER + 1, "app:cosmic-screenshot", 10)
            .unwrap()
            .total,
        0
    );
    service
        .list_source(OWNER, "app:cosmic-screenshot", 10)
        .unwrap()
        .notifications
}

async fn run_case(group: &std::fs::File, case: usize, collision: bool) -> serde_json::Value {
    let product = Product { case, ..CAPTURE };
    let fixture = Fixture::start(case).await;
    let (control, child_control) = bootstrap::pair().unwrap();
    let mut pam = spawn_pam("gui", "native-product", group, Stdio::from(child_control));
    assert_eq!(
        status(&control, &pam, Instant::now() + Duration::from_secs(30)),
        *b"AUTH"
    );
    command(&control, b"CASE");
    command(
        &control,
        format!("{case:02}..").as_bytes().try_into().unwrap(),
    );
    let child = bootstrap::receive(
        control.as_fd(),
        *b"CHLD",
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(
        kernel::sender(child.credentials).unwrap(),
        ProcessIdentity::child(&pam).unwrap()
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let launcher = loop {
        let process = ProcessIdentity::from_pidfd(child.descriptor.as_fd()).unwrap();
        if process.uid() == OWNER {
            break process;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let login = claw_display_control::KernelLogin::of(&launcher).unwrap();
    assert_eq!(login.owner_uid, OWNER);
    let display = crate::display_session::registry::Registry::get()
        .unwrap()
        .for_process(&launcher)
        .unwrap();
    assert_eq!(
        crate::display_session::registry::GuiControl::of(&display)
            .unwrap()
            .owner_uid,
        OWNER
    );
    let parent = launch_parent(&launcher, case);
    command(&control, b"RUN!");
    let approvals = approve_native_startup(&mut pam, product, &parent).await;
    assert_eq!(approvals.len(), 3);
    let deadline = Instant::now() + Duration::from_secs(20);
    let metadata = loop {
        if let Some(request) = crate::approvals::list_pending_for_owner(Some(OWNER))
            .into_iter()
            .find(|request| {
                request.verb == "fs.meta"
                    && request.session.starts_with("app-")
                    && request.operation_digest.is_some()
            })
        {
            break request;
        }
        assert!(
            Instant::now() < deadline,
            "{}",
            diagnostic("Capture window metadata consent")
        );
        assert_eq!(fixture.control.calls.load(Ordering::SeqCst), 0);
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    approve_native_resource(&metadata.id).await;
    let deadline = Instant::now() + Duration::from_secs(25);
    let (session, retained, native) = loop {
        if let Some(session) = bound_gui_session(&parent, product) {
            let retained = retained_instance(&session);
            if let Some(native) = find_native(&retained, product) {
                if case == 39
                    || (fixture.control.calls.load(Ordering::SeqCst) == 1
                        && fixture.control.call.lock().unwrap().is_some())
                {
                    break (session, retained, native);
                }
            }
        }
        assert!(
            pam.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "{}",
            diagnostic("signed Capture private-broker/helper dispatch")
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    startup_authority(&session, &parent, product);
    let guest = std::path::PathBuf::from(format!("/proc/{}/root", native.identity.pid()));
    assert!(
        !guest.join("run/user/62050/bus").exists(),
        "raw owner bus reached the App"
    );
    assert!(!guest
        .join("run/display-home/native capture saved.png")
        .exists());
    let environment = std::fs::read(format!("/proc/{}/environ", native.identity.pid())).unwrap();
    assert!(!environment
        .split(|byte| *byte == 0)
        .any(|entry| entry.starts_with(b"DBUS_SESSION_BUS_ADDRESS=")));
    let runtime =
        Path::new("/run/cos/gui").join(claw_display_control::Epoch(retained.instance.0).label());
    assert!(runtime.is_dir());
    assert!(runtime.join("home-metadata").exists());
    let mut created_target = None;
    if case == 39 {
        let deadline = Instant::now() + Duration::from_secs(20);
        let create = loop {
            if let Some(request) = crate::approvals::list_pending_for_owner(Some(OWNER))
                .into_iter()
                .find(|request| {
                    request.verb == "fs.write"
                        && request.session.starts_with("gui-create-")
                        && request.operation_digest.is_some()
                })
            {
                break request;
            }
            assert!(
                Instant::now() < deadline,
                "{}",
                diagnostic("Capture exact output consent")
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(
            fixture.control.calls.load(Ordering::SeqCst),
            0,
            "capture preceded the exact one-shot output decision"
        );
        let target = create
            .reason
            .strip_prefix("Create ONLY absent regular file ")
            .unwrap()
            .split_once(" once, without replacing an existing target;")
            .unwrap()
            .0;
        assert!(
            target.starts_with("/run/display-home/Pictures/Screenshot_")
                && target.ends_with(".png")
        );
        created_target = Some(std::path::PathBuf::from(target));
        if collision {
            {
                let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
                std::fs::write(target, b"existing output must survive").unwrap();
            }
            approve_native_resource(&create.id).await;
            command(&control, b"WAIT");
            assert_eq!(
                status(&control, &pam, Instant::now() + Duration::from_secs(15)),
                *b"FAIL",
                "racing screenshot output was incorrectly overwritten"
            );
            command(&control, b"CLSE");
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(status) = pam.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                assert!(Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(
                std::fs::read(target).unwrap(),
                b"existing output must survive"
            );
            assert_eq!(fixture.control.calls.load(Ordering::SeqCst), 0);
            assert!(claw_display_control::identity::pidfd_exited(native.pidfd.as_fd()).unwrap());
            assert!(
                claw_display_control::identity::pidfd_exited(retained.process.as_fd()).unwrap()
            );
            assert!(!runtime.exists());
            assert!(
                std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd()))
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .ends_with(b" (deleted)")
            );
            fixture.stop().await;
            return serde_json::json!({"case":case,"racing_target_preserved":true,
                "portal_calls":0,"checked_retired":true});
        }
        approve_native_resource(&create.id).await;
        startup_authority(&session, &parent, product);
        let deadline = Instant::now() + Duration::from_secs(10);
        while fixture.control.call.lock().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "{}",
                diagnostic("Capture image helper dispatch")
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        {
            let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
            std::fs::write(IMAGE, PNG).unwrap();
        }
        fixture
            .response(0, url::Url::from_file_path(IMAGE).unwrap().as_str(), false)
            .await;
    }
    let initial_notifications = if case == 34 { 0 } else { notifications().len() };
    let mut logout = false;
    if case <= 36 {
        fixture
            .response(0, "file:///unrelated-source-must-not-complete", true)
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        native_process(
            &retained,
            &native,
            "unrelated Capture response must not complete",
        );
        assert!(!fixture.helper_exited());
        match case {
            34 => {
                let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
                std::fs::write(SAVED, CONTENT).unwrap();
            }
            35 | 36 => {}
            _ => unreachable!(),
        }
        let uri = if case == 34 {
            url::Url::from_file_path(SAVED).unwrap().to_string()
        } else {
            "clipboard:".into()
        };
        fixture
            .response(if case == 36 { 1 } else { 0 }, &uri, false)
            .await;
    } else if case == 37 {
        let revoking = tokio::spawn(crate::clawd::client::request(
            "/run/cos/clawd.sock",
            crate::clawd::wire::Request::new(
                crate::clawd::routes::Command::PermissionRevoke,
                serde_json::json!({"owner_uid": OWNER, "session": session}),
            ),
        ));
        fixture.wait_close().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !revoking.is_finished(),
            "revocation acknowledged before portal Close"
        );
        assert!(
            runtime.exists(),
            "GUI custody was released before portal Close"
        );
        fixture.control.close_release.notify_one();
        let reply = tokio::time::timeout(Duration::from_secs(10), revoking)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            reply.ok,
            "checked Capture revocation failed: {:?}",
            reply.error
        );
    } else if case == 38 {
        command(&control, b"LOGO");
        fixture.wait_close().await;
        assert!(
            pam.try_wait().unwrap().is_none(),
            "PAM logout preceded portal retirement"
        );
        fixture.control.close_release.notify_one();
        logout = true;
    }
    if !logout {
        command(&control, b"WAIT");
        assert_eq!(
            status(&control, &pam, Instant::now() + Duration::from_secs(15)),
            if case <= 36 || case == 39 {
                *b"PASS"
            } else {
                *b"FAIL"
            },
            "{}",
            diagnostic("Capture launcher outcome")
        );
        command(&control, b"CLSE");
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    let pam_status = loop {
        if let Some(status) = pam.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "{}",
            diagnostic("Capture PAM retirement")
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(
        pam_status.success(),
        "{}",
        diagnostic("Capture PAM outcome")
    );
    assert!(fixture.helper_exited());
    assert!(claw_display_control::identity::pidfd_exited(retained.process.as_fd()).unwrap());
    assert!(claw_display_control::identity::pidfd_exited(native.pidfd.as_fd()).unwrap());
    assert!(!runtime.exists());
    assert!(
        std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd()))
            .unwrap()
            .as_os_str()
            .as_encoded_bytes()
            .ends_with(b" (deleted)")
    );
    assert_eq!(fixture.control.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.control.closes.load(Ordering::SeqCst),
        usize::from(matches!(case, 37 | 38))
    );
    assert_eq!(
        fixture.control.acknowledged.load(Ordering::SeqCst),
        usize::from(matches!(case, 37 | 38))
    );
    let event = completed(&session);
    assert_eq!(
        event["portal_outcome"],
        if case == 34 {
            "saved"
        } else if case == 35 {
            "clipboard"
        } else if case == 39 {
            "image"
        } else {
            "cancelled"
        }
    );
    let notices = notifications();
    assert_eq!(
        notices.len(),
        initial_notifications + usize::from(case <= 35)
    );
    if case <= 35 {
        let notice = notices
            .iter()
            .find(|notice| notice.session_id.as_deref() == Some(&session))
            .unwrap();
        assert_eq!(notice.owner_uid, OWNER);
        assert_eq!(notice.source, "app:cosmic-screenshot");
        assert_eq!(notice.body, if case == 34 { SAVED } else { "" });
        assert!(!notice.title.is_empty());
        let presentation = notice.presentation.as_ref().unwrap();
        assert_eq!(presentation.icon, CAPTURE.desktop_id);
        assert_eq!(presentation.expire_ms, 5000);
        assert!(presentation.transient);
    }
    assert_eq!(std::fs::read(SAVED).unwrap(), CONTENT);
    if let Some(target) = &created_target {
        assert_eq!(std::fs::read(target).unwrap(), PNG);
        let metadata = std::fs::metadata(target).unwrap();
        assert_eq!(metadata.uid(), OWNER);
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.nlink(), 1);
        assert!(
            !Path::new(IMAGE).exists(),
            "OS helper did not consume its private portal image"
        );
    }
    let output = std::fs::read_to_string("/run/gui-fixture/pam-native-product.log").unwrap();
    if case == 34 {
        assert!(
            output.contains(SAVED),
            "the original App did not decode its correlated saved result"
        );
    }
    if case == 36 {
        assert!(output.contains("Screenshot cancelled by user"));
    }
    let helper_pid = fixture
        .control
        .call
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .process
        .pid();
    let report = serde_json::json!({
        "case": case, "session": session, "outcome": event["portal_outcome"],
        "request_id": event["request_id"], "actual_helper_pid": helper_pid,
        "actual_helper_elf": "/usr/local/bin/clawd", "owner_uid": OWNER,
        "protected_startup_decisions": approvals.len(), "raw_app_bus": false,
        "portal_calls": 1, "portal_close_acknowledged": matches!(case, 37 | 38),
        "one_shot_created":created_target,
        "helper_exited": true, "native_exited": true, "cgroup_removed": true,
        "runtime_removed": true, "pam_exited": true,
    });
    fixture.stop().await;
    report
}

pub(super) async fn run(first_group: &std::fs::File) {
    let path = std::fs::read_link(format!("/proc/self/fd/{}", first_group.as_raw_fd())).unwrap();
    let root = path.parent().unwrap().parent().unwrap();
    let mut cases = Vec::new();
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
        std::fs::create_dir("/run/display-home/Pictures").unwrap();
    }
    for case in 34..=39 {
        let group = if case == 34 {
            first_group.try_clone().unwrap()
        } else {
            session_group(root, &case.to_string())
        };
        cases.push(run_case(&group, case, false).await);
        if case == 39 {
            // The product's original screenshot names have one-second precision.
            tokio::time::sleep(Duration::from_secs(1)).await;
            cases.push(run_case(&group, case, true).await);
        }
    }
    std::fs::write(
        "/run/gui-fixture/native-capture-consumer.json",
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "claw.private-native-capture/v1", "cases": cases,
            "proof": "real signed App/PAM/private broker/OS helper and checked retirement",
            "physical_display_or_screen_pixels": false,
        }))
        .unwrap(),
    )
    .unwrap();
    println!("private actual Capture: signed original ELF and public SDK, exact protected GUI reviews, real OS helper, correlated file/clipboard/cancel, blocked-until-Close live revocation and PAM logout, checked process/cgroup/runtime retirement passed; private synthetic portal, NOT physical screenshot/UI proof");
}
