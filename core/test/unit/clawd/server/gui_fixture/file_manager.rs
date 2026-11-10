use super::*;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

const APP: &str = "gui-file-manager-test";
const BUS: &str = "unix:path=/run/gui-public/file-manager-bus/bus";
const SERVICE: &str = "org.freedesktop.FileManager1";
const CLIENT: u32 = OWNER + 1;
const DATA: &str = "/run/display-home/.local/share/cos/apps/gui-file-manager-test";
const AUDIT: &str = "/run/display-home/.local/share/cos/logs/audit.jsonl";

fn vectors() -> serde_json::Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../crates/claw-display-login/test/fixtures/file_manager_intents.json"
    )))
    .unwrap()
}

#[test]
#[ignore = "unprivileged caller subprocess of the private PAM/FileManager1 fixture only"]
fn private_file_manager_client() {
    let uid = unsafe { libc::getuid() };
    assert!(uid == OWNER || uid == CLIENT);
    assert_eq!(unsafe { libc::geteuid() }, uid);
    let marker = Path::new("/run/gui-public/isolated");
    let metadata = std::fs::metadata(marker).unwrap();
    assert_eq!(metadata.uid(), 0);
    assert_eq!(metadata.mode() & 0o022, 0);
    assert_eq!(std::fs::read(marker).unwrap(), b"private-v1");
    for kind in ["mnt", "net"] {
        let current = std::fs::read_link(format!("/proc/self/ns/{kind}")).unwrap();
        let mut recorded = Vec::new();
        for suffix in ["private", "host"] {
            let path = PathBuf::from(format!("/run/gui-public/{kind}-{suffix}"));
            let metadata = std::fs::metadata(&path).unwrap();
            assert_eq!(metadata.uid(), 0);
            assert_eq!(metadata.mode() & 0o022, 0);
            recorded.push(std::fs::read_to_string(path).unwrap());
        }
        assert_eq!(current.to_str().unwrap(), recorded[0].trim());
        assert_ne!(
            recorded[0].trim(),
            recorded[1].trim(),
            "private caller escaped fixture isolation"
        );
    }
    let role = std::env::var("CLAW_GUI_FILE_MANAGER_CALL").unwrap();
    assert_eq!(role == "other", uid == CLIENT);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let address = if uid == OWNER {
                "unix:path=/run/user/62050/bus"
            } else {
                BUS
            };
            let connection = zbus::connection::Builder::address(address)
                .unwrap()
                .build()
                .await
                .expect("private caller could not connect to its fixture bus");
            let service_owner = zbus::fdo::DBusProxy::new(&connection)
                .await
                .unwrap()
                .get_name_owner(SERVICE.try_into().unwrap())
                .await
                .unwrap();
            let result = if role == "exit" {
                connection
                    .call_method(
                        Some("com.clawos.Session"),
                        "/com/clawos/Session",
                        Some("com.clawos.Session"),
                        "Exit",
                        &(),
                    )
                    .await
            } else {
                let vectors = vectors();
                let name = if role == "other" { "folders" } else { &role };
                let call = vectors["calls"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|call| call["name"] == name)
                    .expect("unknown private intent selection");
                let uris: Vec<_> = call["uris"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|uri| uri.as_str().unwrap().to_string())
                    .collect();
                connection
                    .call_method(
                        Some(SERVICE),
                        "/org/freedesktop/FileManager1",
                        Some(SERVICE),
                        call["method"].as_str().unwrap(),
                        &(uris, vectors["startup_id"].as_str().unwrap()),
                    )
                    .await
            };
            if uid == CLIENT {
                assert!(
                    matches!(result, Err(zbus::Error::MethodError(name, _, reply))
                        if name.as_str() == "org.freedesktop.DBus.Error.AccessDenied"
                            && reply.header().sender().is_some_and(|sender|
                                sender.as_str() == service_owner.as_str())),
                    "different-UID FileManager1 caller was not refused"
                );
                println!("private wrong-owner FileManager1 call refused");
            } else {
                assert!(
                    result.is_ok(),
                    "private owner desktop intent was not accepted"
                );
                println!("private same-owner desktop intent accepted, not a GUI outcome");
            }
            connection.close().await.unwrap();
        });
}

struct Process {
    identity: ProcessIdentity,
    descriptor: OwnedFd,
}

fn process(pid: u32) -> Process {
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
    assert!(raw >= 0, "private process pidfd is unavailable");
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    let identity = ProcessIdentity::from_pidfd(descriptor.as_fd()).unwrap();
    Process {
        identity,
        descriptor,
    }
}

fn parent(pid: i32) -> i32 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    stat[stat.rfind(')').unwrap() + 1..]
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

fn cli_child(helper: &Process) -> Process {
    let mut children = Vec::new();
    for task in std::fs::read_dir(format!("/proc/{}/task", helper.identity.pid())).unwrap() {
        let text = std::fs::read_to_string(task.unwrap().path().join("children")).unwrap();
        for pid in text.split_whitespace() {
            let pid: u32 = pid.parse().unwrap();
            if std::fs::read_link(format!("/proc/{pid}/exe"))
                .is_ok_and(|path| path == Path::new("/usr/local/bin/cos"))
            {
                children.push(pid);
            }
        }
    }
    children.sort_unstable();
    children.dedup();
    assert_eq!(
        children.len(),
        1,
        "intent did not use one actual protected cos child"
    );
    let child = process(children[0]);
    assert_eq!(parent(child.identity.pid()), helper.identity.pid());
    assert_eq!(child.identity.uid(), OWNER);
    assert_eq!(
        claw_display_control::KernelLogin::of(&child.identity).unwrap(),
        claw_display_control::KernelLogin::of(&helper.identity).unwrap()
    );
    let environment = std::fs::read(format!("/proc/{}/environ", child.identity.pid())).unwrap();
    assert!(
        !environment
            .windows(b"file-manager-stale-".len())
            .any(|value| value == b"file-manager-stale-"),
        "actual cos borrowed stale App/model environment"
    );
    child
}

struct NoForkTrace {
    pid: i32,
    stopped: bool,
    attached: bool,
}

impl NoForkTrace {
    fn attach(helper: &Process) -> Self {
        helper.identity.assert_current().unwrap();
        let mut trace = Self {
            pid: helper.identity.pid(),
            stopped: false,
            attached: false,
        };
        assert_eq!(
            unsafe {
                libc::ptrace(
                    libc::PTRACE_SEIZE,
                    trace.pid,
                    0_usize,
                    (libc::PTRACE_O_TRACEFORK | libc::PTRACE_O_TRACEVFORK) as usize,
                )
            },
            0,
            "private no-spawn observation requires the existing ptrace facility"
        );
        trace.attached = true;
        trace.interrupt();
        trace.resume(0);
        trace
    }

    fn poll(&mut self) {
        let mut state = 0;
        let result = unsafe { libc::waitpid(self.pid, &mut state, libc::WNOHANG | libc::__WALL) };
        assert!(result >= 0, "private facade trace was lost");
        if result == 0 {
            return;
        }
        assert!(
            libc::WIFSTOPPED(state),
            "private facade exited during denial"
        );
        self.stopped = true;
        let event = state >> 16;
        assert!(
            event != libc::PTRACE_EVENT_FORK && event != libc::PTRACE_EVENT_VFORK,
            "different-UID intent spawned a process before refusal"
        );
        if event != libc::PTRACE_EVENT_STOP {
            self.resume(if event == 0 { libc::WSTOPSIG(state) } else { 0 });
        }
    }

    fn interrupt(&mut self) {
        assert_eq!(
            unsafe { libc::ptrace(libc::PTRACE_INTERRUPT, self.pid, 0_usize, 0_usize) },
            0
        );
        let until = Instant::now() + Duration::from_secs(2);
        while !self.stopped {
            self.poll();
            assert!(Instant::now() < until, "private facade trace stop deadline");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn resume(&mut self, signal: i32) {
        assert_eq!(
            unsafe { libc::ptrace(libc::PTRACE_CONT, self.pid, 0_usize, signal as usize) },
            0
        );
        self.stopped = false;
    }

    fn detach(&mut self) {
        if !self.stopped {
            self.interrupt();
        }
        assert_eq!(
            unsafe { libc::ptrace(libc::PTRACE_DETACH, self.pid, 0_usize, 0_usize) },
            0
        );
        self.attached = false;
        self.stopped = false;
    }
}

impl Drop for NoForkTrace {
    fn drop(&mut self) {
        if !self.attached {
            return;
        }
        if !self.stopped {
            let _ = unsafe { libc::ptrace(libc::PTRACE_INTERRUPT, self.pid, 0_usize, 0_usize) };
            let until = Instant::now() + Duration::from_secs(2);
            while Instant::now() < until {
                let mut state = 0;
                let result =
                    unsafe { libc::waitpid(self.pid, &mut state, libc::WNOHANG | libc::__WALL) };
                if result > 0 && libc::WIFSTOPPED(state) {
                    self.stopped = true;
                    break;
                }
                if result < 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        if self.stopped {
            let _ = unsafe { libc::ptrace(libc::PTRACE_DETACH, self.pid, 0_usize, 0_usize) };
        }
    }
}

fn deny_before_spawn(control: &OwnedFd, pam: &Child, helper: &Process) {
    // ptrace remains on this OS thread throughout the real cross-UID method call.
    let mut trace = NoForkTrace::attach(helper);
    command(control, b"OTHR");
    let until = Instant::now() + Duration::from_secs(12);
    loop {
        trace.poll();
        let mut ready = libc::pollfd {
            fd: control.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut ready, 1, 0) };
        assert!(result >= 0);
        if result > 0 {
            assert_eq!(status(control, pam, until), *b"PASS");
            break;
        }
        assert!(Instant::now() < until, "private wrong-owner call deadline");
        std::thread::sleep(Duration::from_millis(2));
    }
    let until = Instant::now() + Duration::from_millis(250);
    while Instant::now() < until {
        trace.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    trace.detach();
}

fn owner_directory(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(name.as_ptr(), OWNER, OWNER) }, 0);
}

fn configure() {
    assert_eq!(vectors()["app_id"], APP);
    let source = std::fs::read_to_string("/etc/xdg/autostart/claw-file-manager.desktop").unwrap();
    assert!(source.lines().any(|line| line == "OnlyShowIn=COSMIC;"));
    assert!(source.lines().any(|line| line == "X-systemd-skip=true"));
    let exec = "Exec=/usr/lib/cos/bin/claw-file-manager --app cosmic-files";
    assert_eq!(source.lines().filter(|line| *line == exec).count(), 1);
    let config = Path::new("/run/display-home/.config");
    owner_directory(config);
    owner_directory(&config.join("autostart"));
    std::fs::write(
        config.join("autostart/claw-file-manager.desktop"),
        source.replace(
            exec,
            &format!("Exec=/usr/lib/cos/bin/claw-file-manager --app {APP}"),
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        config.join("autostart/claw-file-manager.desktop"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    // Exercise the session-child path even when COSMIC detects systemd.
    std::fs::create_dir_all("/run/systemd/system").unwrap();
}

async fn bound_processes(pam: &mut Child, wrapper: &ProcessIdentity) -> (Process, Process) {
    let connection = zbus::connection::Builder::address(BUS)
        .unwrap()
        .build()
        .await
        .unwrap();
    let bus = zbus::fdo::DBusProxy::new(&connection).await.unwrap();
    let until = Instant::now() + Duration::from_secs(15);
    let helper_name = loop {
        match bus.get_name_owner(SERVICE.try_into().unwrap()).await {
            Ok(name) => break name,
            Err(_) => {
                assert!(
                    pam.try_wait().unwrap().is_none(),
                    "PAM exited before facade startup"
                );
                assert!(
                    Instant::now() < until,
                    "real session-child facade was not available"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    };
    assert_eq!(
        bus.get_connection_unix_user(helper_name.clone().into())
            .await
            .unwrap(),
        OWNER
    );
    let helper = process(
        bus.get_connection_unix_process_id(helper_name.into())
            .await
            .unwrap(),
    );
    let session_name = bus
        .get_name_owner("com.clawos.Session".try_into().unwrap())
        .await
        .unwrap();
    let session = process(
        bus.get_connection_unix_process_id(session_name.into())
            .await
            .unwrap(),
    );
    assert_eq!(parent(helper.identity.pid()), session.identity.pid());
    assert_eq!(parent(session.identity.pid()), wrapper.pid());
    for (process, executable) in [
        (&helper, "/usr/lib/cos/bin/claw-file-manager"),
        (&session, "/usr/bin/cosmic-session"),
    ] {
        assert_eq!(process.identity.uid(), OWNER);
        assert_eq!(
            claw_display_control::KernelLogin::of(&process.identity).unwrap(),
            claw_display_control::KernelLogin::of(wrapper).unwrap()
        );
        assert_eq!(
            std::fs::read_link(format!("/proc/{}/exe", process.identity.pid())).unwrap(),
            Path::new(executable)
        );
    }
    drop(bus);
    connection.close().await.unwrap();
    (helper, session)
}

fn active(name: &str, pam: &mut Child, helper: &Process) -> (String, RetainedInstance, Process) {
    wait_marker(
        &Path::new(DATA).join(format!("file-manager-{name}-ready.json")),
        pam,
    );
    let receipt: serde_json::Value = serde_json::from_slice(
        &std::fs::read(Path::new(DATA).join(format!("file-manager-{name}-ready.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["passed"], true);
    assert_eq!(receipt["literal_argv"], true);
    assert_eq!(receipt["raw_bus_denied"], true);
    assert_eq!(receipt["name"], name);
    if name == "hold" {
        assert_eq!(receipt["retained_descendant"], true);
    }
    let instances = crate::provenance::runtime::running_instances(OWNER).unwrap();
    let session = instances
        .iter()
        .find_map(|(session, instance)| {
            instance
                .package
                .as_ref()
                .filter(|package| package.id == APP)
                .map(|_| session.clone())
        })
        .expect("actual signed FileManager1 GUI instance");
    let info = crate::paths::RoutedPathContext::for_owner(OWNER, "/run/display-home".into())
        .scope_sync(|| crate::proc::session_info_by_id(&session).unwrap());
    assert_eq!(info.app_id.as_deref(), Some(APP));
    assert!(
        info.parent.is_none(),
        "OS intent borrowed a registered App/model parent"
    );
    assert!(
        info.caps.unwrap().is_empty(),
        "zero-needs GUI received extra capabilities"
    );
    let retained = retained_instance(&session);
    let cli = cli_child(helper);
    (session, retained, cli)
}

async fn retired(pam: &mut Child, retained: &RetainedInstance, cli: &Process) {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let group =
            std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd())).unwrap();
        if claw_display_control::identity::pidfd_exited(retained.process.as_fd()).unwrap()
            && claw_display_control::identity::pidfd_exited(cli.descriptor.as_fd()).unwrap()
            && group
                .as_os_str()
                .as_encoded_bytes()
                .ends_with(b" (deleted)")
            && std::fs::read_dir("/run/cos/gui").unwrap().next().is_none()
        {
            return;
        }
        assert!(
            pam.try_wait().unwrap().is_none(),
            "PAM exited before checked GUI retirement"
        );
        assert!(
            Instant::now() < until,
            "actual FileManager1 GUI/cgroup/CLI retirement incomplete"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn checked_outcomes(expected: usize) {
    let entries: Vec<serde_json::Value> = std::fs::read_to_string(AUDIT)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|entry: &serde_json::Value| entry["app"] == APP)
        .collect();
    assert_eq!(
        entries.len(),
        expected,
        "missing actual cos checked GUI outcome"
    );
    for (index, entry) in entries.iter().enumerate() {
        let vectors = vectors();
        if vectors["calls"][index]["name"] == "failure" {
            assert_eq!(entry["status"], "error");
            assert!(
                entry["error"] == "gui_launch_failed",
                "GUI audit failure text was not a static classification"
            );
        } else {
            assert_eq!(entry["status"], "ok");
        }
        let uris = vectors["calls"][index]["uris"].as_array().unwrap();
        let size: usize = uris.iter().map(|uri| uri.as_str().unwrap().len()).sum();
        assert!(
            entry["args"]
                == serde_json::json!([
                    format!("arg_count={}", uris.len()),
                    format!("arg_bytes={size}")
                ]),
            "GUI audit arguments were not counts-only"
        );
    }
}

fn private_logs(directory: &Path, bytes: &mut usize) {
    if !directory.exists() {
        return;
    }
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        if metadata.is_dir() {
            private_logs(&path, bytes);
        } else if metadata.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext == "jsonl" || ext == "log" || ext == "json")
        {
            let content = std::fs::read(path).unwrap();
            *bytes += content.len();
            assert!(
                *bytes <= 64 * 1024 * 1024,
                "private audit fixture grew without a bound"
            );
            for value in [
                b"private-file-manager-intent".as_slice(),
                b"private-intent-secret".as_slice(),
                b"fixture-password".as_slice(),
                b"private-file-manager-startup-do-not-log".as_slice(),
            ] {
                assert!(
                    !content.windows(value.len()).any(|part| part == value),
                    "private GUI intent leaked to a durable log"
                );
            }
        }
    }
}

pub(super) async fn run(root: &Path) {
    configure();
    let group = session_group(root, "file-manager");
    let (control, child_control) = bootstrap::pair().unwrap();
    let mut pam = spawn_pam(
        "session-file-manager",
        "file-manager",
        &group,
        Stdio::from(child_control),
    );
    assert_eq!(
        status(&control, &pam, Instant::now() + Duration::from_secs(30)),
        *b"AUTH"
    );
    let wrapper = bootstrap::receive(
        control.as_fd(),
        *b"SESS",
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap();
    assert_eq!(
        kernel::sender(wrapper.credentials).unwrap(),
        ProcessIdentity::child(&pam).unwrap()
    );
    let until = Instant::now() + Duration::from_secs(5);
    let wrapper_identity = loop {
        let identity = ProcessIdentity::from_pidfd(wrapper.descriptor.as_fd()).unwrap();
        if identity.uid() == OWNER {
            break identity;
        }
        assert!(Instant::now() < until);
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let login = claw_display_control::KernelLogin::of(&wrapper_identity).unwrap();
    assert_eq!(login.owner_uid, OWNER);
    assert_eq!(
        login,
        claw_display_control::KernelLogin::of(&ProcessIdentity::child(&pam).unwrap()).unwrap()
    );
    let (helper, session) = bound_processes(&mut pam, &wrapper_identity).await;
    println!("private FileManager1 actual COSMIC child retains authenticated PAM/kernel ancestry");
    for (index, (tag, name)) in [
        (b"FLDR", "folders"),
        (b"ITEM", "items"),
        (b"PROP", "properties"),
        (b"ERRO", "failure"),
    ]
    .into_iter()
    .enumerate()
    {
        command(&control, tag);
        assert_eq!(
            status(&control, &pam, Instant::now() + Duration::from_secs(12)),
            *b"PASS"
        );
        let (_id, retained, cli) = active(name, &mut pam, &helper);
        std::fs::write(
            Path::new(DATA).join(format!("file-manager-{name}-release")),
            b"release",
        )
        .unwrap();
        retired(&mut pam, &retained, &cli).await;
        checked_outcomes(index + 1);
        println!("private FileManager1 {name}: actual signed GUI and checked Root outcome");
    }
    deny_before_spawn(&control, &pam, &helper);
    checked_outcomes(4);
    println!("private different-UID FileManager1 request: AccessDenied and no fork/vfork");
    command(&control, b"HOLD");
    assert_eq!(
        status(&control, &pam, Instant::now() + Duration::from_secs(12)),
        *b"PASS"
    );
    let (_id, retained, cli) = active("hold", &mut pam, &helper);
    command(&control, b"STOP");
    assert_eq!(
        status(&control, &pam, Instant::now() + Duration::from_secs(20)),
        *b"PASS"
    );
    retired(&mut pam, &retained, &cli).await;
    assert!(claw_display_control::identity::pidfd_exited(helper.descriptor.as_fd()).unwrap());
    assert!(claw_display_control::identity::pidfd_exited(session.descriptor.as_fd()).unwrap());
    assert!(claw_display_control::identity::pidfd_exited(wrapper.descriptor.as_fd()).unwrap());
    command(&control, b"CLSE");
    let until = Instant::now() + Duration::from_secs(15);
    let result = loop {
        if let Some(result) = pam.try_wait().unwrap() {
            break result;
        }
        assert!(
            Instant::now() < until,
            "FileManager1 PAM retirement deadline"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(result.success(), "actual FileManager1 PAM session failed");
    assert!(
        std::fs::read_to_string(root.join("login-file-manager/cgroup.events"))
            .unwrap()
            .lines()
            .any(|line| line == "populated 0")
    );
    assert!(crate::provenance::runtime::running_instances(OWNER)
        .unwrap()
        .iter()
        .all(|(_, instance)| instance
            .package
            .as_ref()
            .is_none_or(|package| package.id != APP)));
    let log = std::fs::read_to_string("/run/gui-fixture/pam-file-manager.log").unwrap();
    assert!(log.contains("private wrong-owner FileManager1 call refused"));
    assert!(log.contains("private PAM retired after actual FileManager1 session forwarding"));
    let mut bytes = 0;
    for root in [
        "/run/display-home/.local/share/cos",
        "/var/lib/cos",
        "/var/log/cos",
        "/run/gui-fixture",
    ] {
        private_logs(Path::new(root), &mut bytes);
    }
    std::fs::remove_dir("/run/systemd/system").unwrap();
    println!("private FileManager1 actual protected cos/signed GUI outcomes, cross-UID no-spawn, private logs and checked session/PAM teardown: passed");
}
