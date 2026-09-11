use super::*;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::path::Path;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::worker::gui_transport::{bootstrap, kernel};
use claw_display_control::ProcessIdentity;

const OWNER: u32 = 62050;

fn command(channel: &OwnedFd, tag: &[u8; 4]) {
    assert_eq!(
        unsafe {
            libc::send(
                channel.as_raw_fd(),
                tag.as_ptr().cast(),
                tag.len(),
                libc::MSG_NOSIGNAL,
            )
        },
        4
    );
}

fn status(channel: &OwnedFd, pam: &Child, deadline: Instant) -> [u8; 4] {
    let mut poll = libc::pollfd {
        fd: channel.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout = deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .min(i32::MAX as u128) as i32;
    assert!(
        unsafe { libc::poll(&mut poll, 1, timeout) } > 0,
        "private PAM response deadline"
    );
    let mut bytes = [0_u8; 5];
    let mut control = [0_usize; 16];
    let mut vector = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    assert_eq!(
        unsafe { libc::recvmsg(channel.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) },
        4
    );
    assert_eq!(message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC), 0);
    let mut identity = None;
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            assert_eq!((*header).cmsg_level, libc::SOL_SOCKET);
            assert_eq!((*header).cmsg_type, libc::SCM_CREDENTIALS);
            assert_eq!(
                (*header).cmsg_len,
                libc::CMSG_LEN(std::mem::size_of::<libc::ucred>() as u32) as usize
            );
            assert!(identity.is_none());
            identity = Some(
                kernel::sender(std::ptr::read_unaligned(
                    libc::CMSG_DATA(header).cast::<libc::ucred>(),
                ))
                .unwrap(),
            );
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    assert_eq!(identity.unwrap(), ProcessIdentity::child(pam).unwrap());
    bytes[..4].try_into().unwrap()
}

fn install_packages() {
    use crate::provenance::{
        envelope::PackageKind,
        sign::{self, SignRequest, SigningKeyFile},
        state::TrustDomain,
        trust::{TrustStore, VENDOR_TRUST_ROOT},
    };
    let root = Path::new(VENDOR_TRUST_ROOT);
    std::fs::create_dir_all(root).unwrap();
    let key =
        SigningKeyFile::generate(Some("private authenticated GUI fixture".to_string())).unwrap();
    std::fs::write(
        root.join("gui-fixture.json"),
        serde_json::to_vec(&key.trust_entry(&[PackageKind::App])).unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(
        root.join("gui-fixture.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    crate::provenance::state::bump(
        root.parent().unwrap(),
        TrustDomain::System,
        &[root.to_path_buf()],
    )
    .unwrap();
    std::fs::set_permissions(
        root.parent().unwrap().join("state.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let roots = TrustStore::default_roots();
    let trust = TrustStore::load_roots(&roots);
    assert!(!trust.is_empty(), "{:?}", trust.diagnostics());
    crate::provenance::set_trust_store_for_roots(trust, roots);
    for id in ["gui-boundary-test", "gui-boundary-zero"] {
        let dir = Path::new("/usr/lib/cos/apps").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::copy("/usr/lib/cos/bin/claw-gui-probe", dir.join("gui-probe")).unwrap();
        std::fs::set_permissions(
            dir.join("gui-probe"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let selection = [
            serde_json::json!({"verb":"clipboard.read","scope":{"kind":"fixed","scope":{"kind":"name","value":"selection"}},"why":{"en":"Read private fixture selection"}}),
            serde_json::json!({"verb":"clipboard.write","scope":{"kind":"fixed","scope":{"kind":"name","value":"selection"}},"why":{"en":"Write private fixture selection"}}),
        ];
        let mut needs = vec![serde_json::json!({
            "verb":"fs.read","scope":{"kind":"fixed","scope":{"kind":"path","value":"/run/gui-public"}},
            "why":{"en":"Inspect the isolated socket-alias fixture"}
        })];
        if id == "gui-boundary-test" {
            needs.extend(selection.clone());
            needs.push(serde_json::json!({
                "verb":"sys.observe","scope":{"kind":"fixed","scope":{"kind":"name","value":"audio"}},
                "why":{"en":"Exercise the existing owner/App deny-and-retire controller"}
            }));
        }
        let manifest = serde_json::json!({
            "id":id, "version":"1.0.0", "schema_version":2,
            "name":{"en":"Private authenticated GUI fixture"},
            "runtime":crate::caps::manifest::Runtime::Binary, "entry":"gui-probe",
            "desktop":{"exec":"surface","panel_applet":true},
            "operations":{"surface":{"label":{"en":"Exercise the GUI resource boundary"},"needs":needs}},
            "mcp":{"entry":"gui-probe","transport":"stdio","tools":[{
                "name":"fixture-selection","summary":{"en":"Must not grant GUI authority"},
                "args":[],"needs":selection
            }]}
        });
        std::fs::write(dir.join("app.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::set_permissions(dir.join("app.json"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        sign::sign_directory(
            &dir,
            &SignRequest {
                kind: PackageKind::App,
                id: id.to_string(),
                version: "1.0.0".to_string(),
                manifest_schema: "integration".to_string(),
                manifest_path: "app.json".to_string(),
                entrypoints: vec!["gui-probe".to_string()],
                resources: vec![],
            },
            &key,
        )
        .unwrap();
        std::fs::set_permissions(
            dir.join(crate::provenance::envelope::ENVELOPE_FILE),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let package = crate::provenance::verify::verify_package(
            &dir,
            &crate::provenance::verify::VerifyOptions::new(PackageKind::App).expect_id(id),
            &crate::provenance::trust_store(),
        )
        .unwrap();
        let review = crate::approvals::system_review::submit(
            OWNER,
            crate::approvals::system_review::ReviewKind::AppActivation,
            &package,
            "private Root fixture controller".to_string(),
        )
        .unwrap();
        crate::approvals::system_review::decide(OWNER, &review.id, true, Some(&package)).unwrap();
    }
}

fn launch_parent(process: &ProcessIdentity, case: usize) -> String {
    let app = if case == 0 {
        "gui-boundary-zero"
    } else {
        "gui-boundary-test"
    };
    let mut caps = CapSet::new();
    caps.insert(Cap::new(Verb::AGENT_INVOKE, Scope::name(app)));
    caps.insert(Cap::new(Verb::FS_READ, Scope::path("/run/gui-public")));
    if [0, 1, 3, 4, 6, 7, 8, 9, 10, 11].contains(&case) {
        caps.insert(Cap::new(Verb::CLIPBOARD_READ, Scope::name("selection")));
    }
    if [0, 2, 3].contains(&case) {
        caps.insert(Cap::new(Verb::CLIPBOARD_WRITE, Scope::name("selection")));
    }
    if case == 11 {
        caps.insert(Cap::new(Verb::SYS_OBSERVE, Scope::name("audio")));
    }
    let session = format!("gui-fixture-parent-{case}");
    let info = crate::proc::SessionInfo {
        session_id: session.clone(),
        pid: process.pid() as u32,
        command: vec!["private authenticated login launcher".to_string()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("user".to_string()),
        parent: None,
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: Some(2),
        scope: None,
        priority: None,
        caps: Some(caps),
        transient_caps: None,
        role: None,
        app_id: None,
        pending_bind: false,
        start_time_ticks: Some(process.start_ticks()),
        client: crate::session::SessionClient::new(
            crate::session::SessionSource::LocalCli,
            true,
            true,
        ),
    };
    crate::paths::RoutedPathContext::for_owner(OWNER, Path::new("/run/display-home").to_path_buf())
        .scope_sync(|| crate::proc::register_session_for_owner(info, OWNER))
        .unwrap();
    session
}

fn wait_marker(path: &Path, pam: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(25);
    while !path.exists() {
        assert!(
            pam.try_wait().unwrap().is_none(),
            "private PAM fixture exited before GUI readiness"
        );
        assert!(
            Instant::now() < deadline,
            "GUI marker missing: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn runtime_layout() {
    use crate::display_session::runtime::{Runtime, Socket};
    let mut runtime = Runtime::create(claw_display_control::InstanceId([0x57; 16])).unwrap();
    let mut listeners = Vec::new();
    for socket in [
        Socket::Wayland,
        Socket::WaylandTransport,
        Socket::Broker,
        Socket::Egress,
        Socket::EgressTransport,
    ] {
        listeners.push(runtime.bind(socket, OWNER).unwrap());
        let mode = std::fs::metadata(runtime.socket(socket).parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode,
            if matches!(socket, Socket::Wayland | Socket::Egress) {
                0o700
            } else {
                0o711
            }
        );
    }
    runtime.remove().unwrap();
    runtime.remove().unwrap();
    assert!(std::fs::read_dir("/run/cos/gui").unwrap().next().is_none());
    drop(listeners);
}

struct RetainedInstance {
    instance: claw_display_control::InstanceId,
    process: OwnedFd,
    group: OwnedFd,
}

fn retained_instance(session: &str) -> RetainedInstance {
    let info = crate::paths::RoutedPathContext::for_owner(
        OWNER,
        Path::new("/run/display-home").to_path_buf(),
    )
    .scope_sync(|| crate::proc::session_info_by_id(session).unwrap());
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, info.pid, 0_u32) };
    assert!(raw >= 0);
    let process = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    let identity = ProcessIdentity::from_pidfd(process.as_fd()).unwrap();
    assert_eq!(identity.uid(), OWNER);
    let group = claw_display_control::workload::SessionGroup::of(&identity)
        .unwrap()
        .open()
        .unwrap();
    let entries: Vec<_> = std::fs::read_dir("/run/cos/gui")
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(entries.len(), 1);
    let name = &entries[0];
    assert_eq!(name.len(), 32);
    let mut bytes = [0_u8; 16];
    for (index, value) in bytes.iter_mut().enumerate() {
        *value = u8::from_str_radix(&name[index * 2..index * 2 + 2], 16).unwrap();
    }
    RetainedInstance {
        instance: claw_display_control::InstanceId(bytes),
        process,
        group,
    }
}

fn creator_refusals(
    control: &crate::display_session::registry::GuiControl,
    retained: &RetainedInstance,
    retired: bool,
) {
    use claw_display_control::{
        wire::{monotonic_ms, GuiRefusal},
        CompositorCommand, CompositorReply, Epoch, InstanceId,
    };
    for case in if retired { 4..5 } else { 0..4 } {
        let (server, _client) = std::os::unix::net::UnixStream::pair().unwrap();
        let instance = if retired {
            retained.instance
        } else {
            InstanceId([0x80 + case; 16])
        };
        let process = if case == 2 {
            ProcessIdentity::current().unwrap().pidfd().unwrap()
        } else {
            retained.process.try_clone().unwrap()
        };
        let command = CompositorCommand::Creator {
            epoch: control.epoch,
            owner_uid: if case == 0 { OWNER + 1 } else { OWNER },
            instance,
            authority: if case == 1 {
                Epoch([0; 16])
            } else {
                Epoch([0x62; 16])
            },
            revision: 1,
            expires_monotonic_ms: if case == 3 {
                1
            } else {
                monotonic_ms().unwrap() + 1000
            },
            selection_read: false,
            selection_write: false,
            layer_shell: false,
        };
        let reply = control
            .exchange(
                command,
                vec![server.into(), process, retained.group.try_clone().unwrap()],
                Instant::now() + Duration::from_secs(2),
            )
            .unwrap();
        assert!(
            matches!(reply, CompositorReply::Rejected {
                epoch, instance: received, reason
            } if epoch == control.epoch && received == instance && if case == 3 {
                matches!(reason, GuiRefusal::Expired)
            } else {
                matches!(reason, GuiRefusal::InvalidBinding)
            }),
            "private creator refusal case {case}: {reply:?}"
        );
    }
}

fn spawn_pam(mode: &str, label: &str, group: &std::fs::File, input: Stdio) -> Child {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("/run/gui-fixture/pam-{label}.log"))
        .unwrap();
    let mut pam_command = ProcessCommand::new("/usr/lib/cos/bin/pam-login-fixture");
    pam_command
        .args(["/run/gui-fixture/pam", mode])
        .stdin(input)
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log));
    let membership = group.as_raw_fd();
    unsafe {
        pam_command.pre_exec(move || {
            if libc::write(membership, b"0".as_ptr().cast(), 1) != 1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    pam_command.spawn().unwrap()
}

fn session_group(root: &Path, label: &str) -> std::fs::File {
    let path = root.join(format!("login-{label}"));
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(path.join("cgroup.procs"))
        .unwrap()
}

fn activation_cases(root: &Path) {
    for (mode, label, changed, expected) in [
        ("deny-password", "password", None, 1),
        ("non-root-open", "non-root", None, 1),
        ("invalid-locale", "locale", None, 1),
        (
            "authenticate-only",
            "host-parent",
            Some(("/usr/lib/cos/bin", 0o777)),
            1,
        ),
        (
            "authenticate-only",
            "compositor-mode",
            Some(("/usr/bin/cosmic-comp", 0o644)),
            1,
        ),
        ("duplicate-open", "duplicate", None, 0),
        ("inherited-handle", "inherited", None, 0),
        ("subscribe-reuse", "subscribers", None, 0),
    ] {
        let original = changed.map(|(path, mode)| {
            let permissions = std::fs::metadata(path).unwrap().permissions();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
            (path, permissions)
        });
        let group = session_group(root, label);
        let mut child = spawn_pam(mode, label, &group, Stdio::null());
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("private activation case {label} exceeded its deadline");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if let Some((path, permissions)) = original {
            std::fs::set_permissions(path, permissions).unwrap();
        }
        assert_eq!(
            status.code(),
            Some(expected),
            "private activation case {label}: {}",
            std::fs::read_to_string(format!("/run/gui-fixture/pam-{label}.log")).unwrap(),
        );
        println!("private authenticated activation case {label}: passed");
    }
}

fn session_startup_failure(root: &Path) {
    let group = session_group(root, "session-startup");
    let mut pam = spawn_pam(
        "session-startup-failure",
        "session-startup",
        &group,
        Stdio::null(),
    );
    let deadline = Instant::now() + Duration::from_secs(25);
    let status = loop {
        if let Some(status) = pam.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            pam.kill().unwrap();
            pam.wait().unwrap();
            panic!(
                "private session/PAM exit exceeded its deadline: {}",
                std::fs::read_to_string("/run/gui-fixture/pam-session-startup.log").unwrap(),
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let log = std::fs::read_to_string("/run/gui-fixture/pam-session-startup.log").unwrap();
    assert!(
        log.contains("got environmental variables from cosmic-comp")
            && log.contains("failed to start settings daemon"),
        "the actual session must attach its display and retain the startup diagnostic: {log}",
    );
    assert_eq!(
        status.code(),
        Some(0),
        "session startup failure must not block session/PAM exit: {log}",
    );
    for evidence in [
        "private authenticated display observer is ready",
        "private cosmic-session exited with startup panic: 101",
        "private authenticated display subscription remained alive after startup failure",
        "private PAM retired after cosmic-session startup failure",
    ] {
        assert!(log.contains(evidence), "missing {evidence}: {log}");
    }
    assert!(
        std::fs::read_to_string(root.join("login-session-startup/cgroup.events"))
            .unwrap()
            .lines()
            .any(|line| line == "populated 0"),
        "private session/PAM processes remain after startup failure",
    );
    println!("private actual cosmic-session startup failure and checked PAM exit: passed");
}

async fn owner_permissions(params: serde_json::Value) -> std::process::Output {
    let mut command = ProcessCommand::new("/usr/local/bin/cos");
    command
        .args(["__app-permissions", &params.to_string()])
        .env_clear()
        .env("HOME", "/run/display-home")
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .current_dir("/run/display-home")
        .stdin(Stdio::null());
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
    tokio::time::timeout(
        Duration::from_secs(15),
        command.output(),
    )
    .await
    .expect("owner permission CLI exceeded its deadline")
    .unwrap()
}

async fn run_case(case: usize, group: &std::fs::File) {
    let (control, child_control) = bootstrap::pair().unwrap();
    let mut pam = spawn_pam("gui", &case.to_string(), group, Stdio::from(child_control));
    assert_eq!(
        status(&control, &pam, Instant::now() + Duration::from_secs(30)),
        *b"AUTH"
    );
    println!("private GUI case {case}: authenticated PAM display");
    let host = if [8, 9].contains(&case) {
        command(&control, b"HOST");
        let host = bootstrap::receive(
            control.as_fd(),
            *b"HOST",
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(
            kernel::sender(host.credentials).unwrap(),
            ProcessIdentity::child(&pam).unwrap(),
        );
        let process = ProcessIdentity::from_pidfd(host.descriptor.as_fd()).unwrap();
        assert_eq!(process.uid(), 0);
        assert_eq!(
            claw_display_control::KernelLogin::of(&process).unwrap(),
            claw_display_control::KernelLogin::of(&ProcessIdentity::child(&pam).unwrap()).unwrap(),
        );
        Some(host.descriptor)
    } else {
        None
    };
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
        let current = ProcessIdentity::from_pidfd(child.descriptor.as_fd()).unwrap();
        if current.uid() == OWNER && current.gid() == OWNER {
            break current;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    let login = claw_display_control::identity::KernelLogin::of(&launcher).unwrap();
    assert_eq!(login.owner_uid, OWNER);
    let parent = launch_parent(&launcher, case);
    println!("private GUI case {case}: exact launcher authority bound");
    let display = crate::display_session::registry::Registry::get()
        .unwrap()
        .for_process(&launcher)
        .unwrap();
    let display = crate::display_session::registry::GuiControl::of(&display).unwrap();
    let data = Path::new("/run/display-home/.local/share/cos/apps/gui-boundary-test");
    if [4, 6, 7, 8, 9, 10, 11].contains(&case) {
        match std::fs::remove_file(data.join("revoke-ready")) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("clear previous private GUI marker: {error}"),
        }
    }
    command(&control, b"RUN!");
    if [4, 6, 7, 8, 9, 10, 11].contains(&case) {
        wait_marker(&data.join("revoke-ready"), &mut pam);
        let instances = crate::provenance::runtime::running_instances(OWNER).unwrap();
        let (session, _) = instances
            .iter()
            .find(|(_, instance)| {
                instance
                    .package
                    .as_ref()
                    .is_some_and(|package| package.id == "gui-boundary-test")
            })
            .expect("actual signed GUI instance");
        let retained = retained_instance(session);
        match case {
            4 => {
                creator_refusals(&display, &retained, false);
                crate::clawd::authority::authority().revoke_session(session);
                crate::clawd::gui::retire_session(
                    OWNER,
                    session,
                    Instant::now() + Duration::from_secs(5),
                )
                .unwrap();
            }
            6 => {
                crate::paths::RoutedPathContext::for_owner(
                    OWNER,
                    PathBuf::from("/run/display-home"),
                )
                .scope_sync(|| {
                    let mut info = crate::proc::session_info_by_id(&parent).unwrap();
                    info.caps = Some(CapSet::new());
                    crate::proc::register_session_for_owner(info, OWNER).unwrap();
                });
                let deadline = Instant::now() + Duration::from_secs(4);
                while !claw_display_control::identity::pidfd_exited(retained.process.as_fd())
                    .unwrap()
                {
                    assert!(
                        Instant::now() < deadline,
                        "lost parent authority did not retire GUI"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                crate::clawd::gui::retire_session(OWNER, &parent, deadline).unwrap();
            }
            7 | 10 => {
                if case == 10 {
                    for params in [
                        serde_json::json!({"session": parent}),
                        serde_json::json!({}),
                        serde_json::json!({"owner_uid": 0, "session": parent}),
                        serde_json::json!({"owner_uid": 0}),
                        serde_json::json!({"owner_uid": OWNER + 1, "session": parent}),
                    ] {
                        let response = crate::clawd::client::request(
                            "/run/cos/clawd.sock",
                            crate::clawd::wire::Request::new(
                                crate::clawd::routes::Command::PermissionRevoke,
                                params,
                            ),
                        )
                        .await
                        .unwrap();
                        assert!(response.ok, "isolated bucket revocation failed: {:?}", response.error);
                        assert!(!claw_display_control::identity::pidfd_exited(retained.process.as_fd()).unwrap());
                    }
                    tokio::time::sleep(Duration::from_millis(1200)).await;
                    assert!(!claw_display_control::identity::pidfd_exited(retained.process.as_fd()).unwrap());
                }
                let response = crate::clawd::client::request(
                    "/run/cos/clawd.sock",
                    crate::clawd::wire::Request::new(
                        crate::clawd::routes::Command::PermissionRevoke,
                        serde_json::json!({"owner_uid": OWNER, "session": parent}),
                    ),
                )
                .await
                .unwrap();
                assert!(
                    response.ok,
                    "authenticated revocation route failed: {:?}",
                    response.error
                );
                assert_eq!(response.result.unwrap()["revoked"], true);
            }
            11 => {
                let shown = owner_permissions(serde_json::json!({
                    "action":"show", "app_id":"gui-boundary-test"
                }))
                .await;
                assert!(shown.status.success(), "{}", String::from_utf8_lossy(&shown.stderr));
                let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
                let permission = shown["permissions"]
                    .as_array().unwrap()
                    .iter()
                    .find(|permission| permission["capability"]["verb"] == "sys.observe")
                    .unwrap();
                assert_eq!(permission["manageable"], true);
                assert_eq!(permission["enabled"], true);
                assert_eq!(permission["live_granted"], true);
                let audit = crate::paths::RoutedPathContext::for_owner(
                    OWNER, PathBuf::from("/run/display-home"),
                )
                .scope_sync(|| crate::paths::data_dir().join("clawd/audit.jsonl"));
                let retained_audit = audit.with_file_name("audit-before-gui-revoke.jsonl");
                std::fs::rename(&audit, &retained_audit).unwrap();
                std::fs::create_dir(&audit).unwrap();
                let denied = owner_permissions(serde_json::json!({
                    "action":"revoke", "app_id":"gui-boundary-test",
                    "permission_id":permission["permission_id"],
                }))
                .await;
                std::fs::remove_dir(&audit).unwrap();
                std::fs::rename(&retained_audit, &audit).unwrap();
                assert!(!denied.status.success());
                let denied: serde_json::Value = serde_json::from_slice(&denied.stdout)
                    .unwrap_or_else(|error| panic!("invalid CLI error response: {error}; {denied:?}"));
                let diagnostic = denied["error"].as_str().expect("CLI error string");
                assert!(diagnostic.contains("was disabled") && diagnostic.contains("revocation audit failed"), "{diagnostic}");
                assert!(!diagnostic.contains("GUI retirement did not complete"), "{diagnostic}");
                assert!(claw_display_control::identity::pidfd_exited(retained.process.as_fd()).unwrap());
                let denied_cap = CapSet::from_caps([Cap::new(Verb::SYS_OBSERVE, Scope::name("audio"))]);
                crate::paths::RoutedPathContext::for_owner(OWNER, PathBuf::from("/run/display-home"))
                    .scope_sync(|| {
                        assert!(crate::approvals::app_policy::require(
                            OWNER, "gui-boundary-test", &denied_cap,
                        ).is_err());
                    });
            }
            8 | 9 => {
                let host = host.as_ref().unwrap();
                assert_eq!(
                    unsafe {
                        libc::syscall(
                            libc::SYS_pidfd_send_signal,
                            host.as_raw_fd(),
                            if case == 8 {
                                libc::SIGKILL
                            } else {
                                libc::SIGSTOP
                            },
                            std::ptr::null::<libc::siginfo_t>(),
                            0_u32,
                        )
                    },
                    0
                );
                let deadline = Instant::now() + Duration::from_secs(7);
                while !claw_display_control::identity::pidfd_exited(retained.process.as_fd())
                    .unwrap()
                {
                    assert!(
                        Instant::now() < deadline,
                        "lost display control left the GUI alive"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                if case == 9 && !claw_display_control::identity::pidfd_exited(host.as_fd()).unwrap()
                {
                    assert_eq!(
                        unsafe {
                            libc::syscall(
                                libc::SYS_pidfd_send_signal,
                                host.as_raw_fd(),
                                libc::SIGCONT,
                                std::ptr::null::<libc::siginfo_t>(),
                                0_u32,
                            )
                        },
                        0
                    );
                }
                crate::clawd::gui::retire_session(
                    OWNER,
                    session,
                    Instant::now() + Duration::from_secs(5),
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(claw_display_control::identity::pidfd_exited(retained.process.as_fd()).unwrap());
        let group_link =
            std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd())).unwrap();
        assert!(
            group_link
                .as_os_str()
                .as_encoded_bytes()
                .ends_with(b" (deleted)"),
            "the exact checked-empty cgroup must be detached: {}",
            group_link.display(),
        );
        if case == 4 {
            creator_refusals(&display, &retained, true);
        }
    }
    if case == 5 {
        wait_marker(&data.join("aliases-ready"), &mut pam);
        let backend =
            std::os::unix::net::UnixListener::bind("/run/gui-public/owner-bus.sock").unwrap();
        backend.set_nonblocking(true).unwrap();
        std::fs::hard_link(
            "/run/gui-public/owner-bus.sock",
            "/run/gui-public/owner-bus-alias.sock",
        )
        .unwrap();
        let ordinary = std::fs::read_dir("/run/user/62050")
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("wayland-")
                    && !path
                        .extension()
                        .is_some_and(|extension| extension == "lock")
            })
            .expect("actual private compositor socket");
        std::fs::hard_link(ordinary, "/run/gui-public/ordinary-wayland.sock").unwrap();
        std::fs::write("/run/gui-public/ready", b"ready").unwrap();
        command(&control, b"WAIT");
        assert_eq!(
            status(&control, &pam, Instant::now() + Duration::from_secs(30)),
            *b"PASS"
        );
        assert_eq!(
            backend.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    } else {
        command(&control, b"WAIT");
        let expected = if [4, 6, 7, 8, 9, 10, 11].contains(&case) {
            *b"FAIL"
        } else {
            *b"PASS"
        };
        assert_eq!(
            status(&control, &pam, Instant::now() + Duration::from_secs(30)),
            expected,
            "case {case}: {}",
            std::fs::read_to_string(format!("/run/gui-fixture/pam-{case}.log")).unwrap()
        );
    }
    command(&control, b"CLSE");
    let pam_status = pam.wait().unwrap();
    if [8, 9].contains(&case) {
        assert_eq!(
            pam_status.code(),
            Some(4),
            "lost supervisor must be an explicit PAM failure"
        );
    } else {
        assert!(pam_status.success());
    }
    if case == 2 {
        let log = std::fs::read_to_string("/run/gui-fixture/pam-2.log").unwrap();
        assert!(
            log.lines()
                .any(|line| line == "private write-only witness: 6 payloads, 6 clears"),
            "write-only needs an actual independent compositor-side payload/clear witness: {log}",
        );
    }
    crate::paths::RoutedPathContext::for_owner(OWNER, Path::new("/run/display-home").to_path_buf())
        .scope_sync(|| crate::proc::deregister_session(&parent));
    assert!(std::fs::read_dir("/run/cos/gui").unwrap().next().is_none());
    println!("private authenticated GUI case {case}: passed");
}

fn headless_cli(group: &std::fs::File) {
    let invoke = |args: &[&str]| {
        let mut command = ProcessCommand::new("/usr/local/bin/cos");
        command
            .args(args)
            .env_clear()
            .env("HOME", "/run/display-home")
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let membership = group.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                if libc::write(membership, b"0".as_ptr().cast(), 1) != 1
                    || libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(OWNER) != 0
                    || libc::setuid(OWNER) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!("headless CLI exceeded its deadline: {:?}", output);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.wait_with_output().unwrap()
    };
    assert!(invoke(&["--help"]).status.success());
    let gui = invoke(&[
        "app",
        "gui-boundary-zero",
        "surface",
        "zero",
        "literal ; quote' --flag",
        "",
    ]);
    assert!(!gui.status.success());
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&gui.stdout),
        String::from_utf8_lossy(&gui.stderr),
    );
    assert!(
        diagnostic.contains("login") || diagnostic.contains("display"),
        "headless GUI refusal must identify the missing display boundary: {diagnostic}",
    );
    assert!(crate::provenance::runtime::running_instances(OWNER)
        .unwrap()
        .is_empty());
    println!("private headless CLI remains usable and refuses unbound GUI execution");
}

#[test]
#[ignore = "requires the private authenticated Root PAM/display fixture namespace"]
fn private_authenticated_gui() {
    private_gui_fixture(false);
}

#[test]
#[ignore = "requires the private authenticated Root PAM/display fixture and cosmic-session"]
fn private_session_startup_failure() {
    private_gui_fixture(true);
}

fn private_gui_fixture(session_only: bool) {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert_ne!(
        std::fs::read_link("/proc/self/ns/mnt").unwrap(),
        std::fs::read_link("/proc/1/ns/mnt").unwrap()
    );
    assert_eq!(
        std::fs::read("/run/gui-fixture/isolated").unwrap(),
        b"private-v1"
    );
    assert_eq!(std::env::var("HOME").unwrap(), "/root");
    crate::agentd::guard::mark_broker_process();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .try_init()
        .unwrap();
    crate::storage::set_private_umask();
    crate::storage::harden_clawd_runtime().unwrap();
    crate::storage::harden_clawd_state().unwrap();
    runtime_layout();
    install_packages();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let state = DaemonState::try_new().unwrap();
        let admission = Admission::new(Limits::default());
        let _manager = crate::clawd::gui::Manager::start(state.clone(), admission.clone()).unwrap();
        let _registry = crate::display_session::registry::Registry::start().unwrap();
        let socket = Path::new("/run/cos/clawd.sock");
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        enable_credential_passing(&listener, socket).unwrap();
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o666)).unwrap();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let state = state.clone();
                let admission = admission.clone();
                tokio::spawn(async move {
                    serve_connection(stream, state, admission).await;
                });
            }
        });
        let path = std::fs::read_to_string("/run/gui-fixture/cgroup").unwrap();
        let path = path.trim();
        assert!(path.starts_with("/sys/fs/cgroup/claw-display-fixture."));
        let root = Path::new(path);
        std::fs::write(root.join("cgroup.subtree_control"), b"+cpu +memory +pids").unwrap();
        if !session_only {
            headless_cli(&session_group(root, "headless"));
            activation_cases(root);
            for case in 0..12 {
                let group = session_group(root, &case.to_string());
                run_case(case, &group).await;
            }
        }
        session_startup_failure(root);
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    });
}
