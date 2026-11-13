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

mod package {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/clawd/server/gui_fixture/package.rs"));
}

mod file_manager {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/server/gui_fixture/file_manager.rs"
    ));
}

mod native_products {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/server/gui_fixture/native_products.rs"
    ));
}

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

fn install_packages(package_only: bool, native: Option<native_products::Product>) {
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
    if let Some(product) = native {
        native_products::install(&key, product);
        return;
    }
    eprintln!(
        "private selected-read timing sourceStep=fixture_probe_elf_bytes probeElfBytes={}",
        std::fs::metadata("/usr/lib/cos/bin/claw-gui-probe").unwrap().len(),
    );
    for id in ["gui-boundary-test", "gui-boundary-zero", "gui-file-manager-test"] {
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
        let mut desktop = serde_json::json!({"exec":"surface","panel_applet":true});
        if id == "gui-file-manager-test" {
            desktop = serde_json::json!({
                "exec":"file-manager-surface","needs":[],"panel_applet":false,
            });
        }
        let mut entrypoints = vec!["gui-probe".to_string()];
        if id == "gui-boundary-test" {
            std::fs::copy(dir.join("gui-probe"), dir.join("gui-widget")).unwrap();
            std::fs::set_permissions(
                dir.join("gui-widget"), std::fs::Permissions::from_mode(0o755),
            ).unwrap();
            desktop["entries"] = serde_json::json!({"applet":{
                "entry":"gui-widget","exec":"surface","name":{"en":"Independent widget"},"needs":[]
            }});
            desktop["entries"]["home-metadata"] = serde_json::json!({
                "entry":"gui-probe", "exec":"surface", "name":{"en":"Private Home metadata"},
                "needs":[{"verb":"fs.meta","scope":{"kind":"owner-path","path":"**"},
                    "why":{"en":"Browse Home names and metadata for this window, without content access"}}]
            });
            desktop["entries"]["metadata-subroot"] = serde_json::json!({
                "entry":"gui-probe", "exec":"surface", "name":{"en":"Private generic directory metadata"},
                "needs":[
                    {"verb":"fs.meta","scope":{"kind":"owner-path","path":"Projects/review/**"},
                        "why":{"en":"Browse one declared Home subroot without content access"}},
                    {"verb":"ui.panel","scope":{"kind":"fixed","scope":{"kind":"wild"}},
                        "why":{"en":"Independently exercise the ordinary panel capability"}}
                ],
                "optional_needs":[
                    {"verb":"fs.read","scope":{"kind":"owner-path","path":"Projects/review/**"},
                        "why":{"en":"Read only a separately selected file inside the declared subroot"}},
                    {"verb":"fs.write","scope":{"kind":"owner-path","path":"Projects/review/**"},
                        "why":{"en":"Keep write eligibility separate from metadata and read"}},
                    {"verb":"fs.watch","scope":{"kind":"owner-path","path":"Projects/review/**"},
                        "why":{"en":"Keep watch eligibility separate from metadata"}},
                    {"verb":"fs.delete","scope":{"kind":"owner-path","path":"Projects/review/**"},
                        "why":{"en":"Keep deletion eligibility separate from metadata"}}
                ]
            });
            desktop["entries"]["selected-read"] = serde_json::json!({
                "entry":"gui-probe", "exec":"surface", "name":{"en":"Private selected binary read"},
                "needs":[
                    {"verb":"fs.meta","scope":{"kind":"owner-path","path":"**"},
                        "why":{"en":"Browse Home metadata for this window, without content selection"}},
                    {"verb":"fs.read","scope":{"kind":"owner-path","path":"read-delegated/**"},
                        "why":{"en":"Exercise inherited read capability without implicit content selection"}}
                ],
                "optional_needs":[
                    {"verb":"fs.read","scope":{"kind":"owner-path","path":"**"},
                        "why":{"en":"Request protected owner consent for one selected file after startup"}},
                    {"verb":"fs.write","scope":{"kind":"owner-path","path":"**"},
                        "why":{"en":"Exercise capability authority without production native writes"}},
                    {"verb":"fs.watch","scope":{"kind":"owner-path","path":"**"},
                        "why":{"en":"Exercise capability authority without production native watches"}},
                    {"verb":"fs.delete","scope":{"kind":"owner-path","path":"**"},
                        "why":{"en":"Exercise capability authority without production deletion"}}
                ]
            });
            desktop["entries"]["selected-write"] = serde_json::json!({
                "entry":"gui-probe", "exec":"surface", "name":{"en":"Private selected writes"},
                "needs":[{"verb":"fs.meta","scope":{"kind":"owner-path","path":"**"},
                    "why":{"en":"Browse metadata before independent content decisions"}}],
                "optional_needs":[
                    {"verb":"fs.read","scope":{"kind":"owner-path","path":"**"},"why":{"en":"Read one owner-selected file"}},
                    {"verb":"fs.write","scope":{"kind":"owner-path","path":"**"},"why":{"en":"Write or atomically replace one owner-selected file"}}
                ]
            });
            desktop["entries"]["selected-create"] = serde_json::json!({
                "entry":"gui-probe","exec":"surface","name":{"en":"Private one-shot file creation"},
                "needs":[{"verb":"fs.meta","scope":{"kind":"owner-path","path":"Projects/**"},
                    "why":{"en":"Browse only the exact creation parent subroot"}}],
                "optional_needs":[
                    {"verb":"fs.read","scope":{"kind":"owner-path","path":"Projects/**"},"why":{"en":"Independent read eligibility"}},
                    {"verb":"fs.write","scope":{"kind":"owner-path","path":"Projects/**"},"why":{"en":"Protected one-shot file creation"}}
                ]
            });
            if package_only {
                let optional = [
                    "claw-app-acceptance-support", "claw-package-acceptance-side-effect",
                    "claw-native-package-unused", "claw-native-package-pending",
                ].into_iter().map(|name| serde_json::json!({
                    "verb":"sys.package", "scope":{"kind":"fixed","scope":{"kind":"name","value":name}},
                    "why":{"en":"Request this exact private Debian target through protected Critical OS review"}
                })).collect::<Vec<_>>();
                desktop["entries"]["package"] = serde_json::json!({
                    "entry":"gui-probe", "exec":"surface", "name":{"en":"Private retained package handoff"},
                    "needs":[], "optional_needs":optional,
                });
            }
            for (name, label, surface_needs) in [
                ("read", "Private read-only selection", vec![needs[0].clone(), selection[0].clone()]),
                ("write", "Private write-only selection", vec![needs[0].clone(), selection[1].clone()]),
                ("both", "Private independent selection rights",
                    vec![needs[0].clone(), selection[0].clone(), selection[1].clone()]),
                ("aliases", "Private transport-alias refusal", vec![needs[0].clone()]),
                ("read-observe", "Private owner-policy retirement",
                    vec![needs[0].clone(), selection[0].clone(), needs.last().unwrap().clone()]),
            ] {
                desktop["entries"][name] = serde_json::json!({
                    "entry":"gui-probe", "exec":"surface", "name":{"en":label},
                    "needs":surface_needs
                });
            }
            let panel = serde_json::json!({
                "verb":"ui.panel","scope":{"kind":"fixed","scope":{"kind":"wild"}},
                "why":{"en":"Show the private overlay fixture"}
            });
            for (name, needs) in [
                ("panel", vec![panel.clone()]),
                ("panel-read", vec![panel.clone(), selection[0].clone()]),
                ("panel-write", vec![panel, selection[1].clone()]),
                ("window", vec![serde_json::json!({
                    "verb":"ui.window","scope":{"kind":"fixed","scope":{"kind":"wild"}},
                    "why":{"en":"Show an ordinary private fixture window"}
                })]),
            ] {
                desktop["entries"][name] = serde_json::json!({
                    "entry":"gui-widget","exec":"surface","name":{"en":"Private presentation"},
                    "panel_applet":true,"needs":needs
                });
            }
            entrypoints.push("gui-widget".to_string());
        }
        let manifest = serde_json::json!({
            "id":id, "version":"1.0.0", "schema_version":2,
            "name":{"en":"Private authenticated GUI fixture"},
            "runtime":crate::caps::manifest::Runtime::Binary, "entry":"gui-probe",
            "desktop":desktop,
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
                entrypoints,
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
    if package_only { package::install_denied_app(&key); }
}

fn launch_parent(process: &ProcessIdentity, case: usize) -> String {
    let app = if case == 25 {
        "cosmic-edit"
    } else if case == 26 {
        "cosmic-files"
    } else if case == 27 {
        "cosmic-term"
    } else if case == 28 {
        "panel-calendar"
    } else if case == 29 {
        "cosmic-store"
    } else if case == 0 {
        "gui-boundary-zero"
    } else {
        "gui-boundary-test"
    };
    let mut caps = CapSet::new();
    caps.insert(Cap::new(Verb::AGENT_INVOKE, Scope::name(app)));
    if ![25, 26, 27, 28, 29].contains(&case) {
        caps.insert(Cap::new(Verb::FS_READ, Scope::path("/run/gui-public")));
    }
    if [0, 1, 3, 4, 6, 7, 8, 9, 10, 11, 14, 17, 18].contains(&case) {
        caps.insert(Cap::new(Verb::CLIPBOARD_READ, Scope::name("selection")));
    }
    if [0, 2, 3, 15].contains(&case) {
        caps.insert(Cap::new(Verb::CLIPBOARD_WRITE, Scope::name("selection")));
    }
    if [13, 14, 15, 17, 18, 23].contains(&case) {
        caps.insert(Cap::unscoped(Verb::UI_PANEL));
    }
    if case == 16 {
        caps.insert(Cap::unscoped(Verb::UI_WINDOW));
    }
    if case == 11 {
        caps.insert(Cap::new(Verb::SYS_OBSERVE, Scope::name("audio")));
    }
    if [19, 20, 21, 23, 25, 26].contains(&case) {
        caps.insert(Cap::new(Verb::FS_META, Scope::path("/run/display-home/**")));
    }
    if case == 20 {
        caps.insert(Cap::new(Verb::FS_READ, Scope::path("/run/display-home/read-delegated/**")));
    }
    if case == 24 {
        caps.insert(Cap::new(Verb::FS_META, Scope::path("/run/display-home/Projects/**")));
    }
    let session = if case == 20 || case == 23 || case == 24 {
        format!("gui-fixture-parent-{case}-{}", process.pid())
    } else {
        format!("gui-fixture-parent-{case}")
    };
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

fn private_audit_tail() -> String {
    use std::io::{Read, Seek, SeekFrom};

    let mut output = String::new();
    for kind in ["selected-read", "selected-write"] {
        if let Ok(file) = std::fs::File::open(
            Path::new("/run/display-home/.local/share/cos/apps/gui-boundary-test")
                .join(format!("{kind}-timing.jsonl")),
        ) {
            let mut bytes = Vec::new();
            if file.take(4096).read_to_end(&mut bytes).is_ok() {
                for line in String::from_utf8_lossy(&bytes).lines().take(16) {
                    let Ok(timing) = serde_json::from_str::<serde_json::Value>(line) else { continue; };
                    let (Some(step), Some(ms)) = (
                        timing.get("sourceStep").and_then(serde_json::Value::as_str),
                        timing.get("elapsedMs").and_then(serde_json::Value::as_u64),
                    ) else { continue; };
                    if matches!(step, "request_pending" | "status_selected" | "native_file_opened"
                        | "native_read_complete" | "seek_mmap_complete" | "scalar_denial_done"
                        | "pending_sibling_cleanup_ready" | "write_read_request_pending"
                        | "write_read_selected" | "write_requests_pending" | "write_selected"
                        | "write_only_complete" | "original_read_complete" | "direct_write_synced"
                        | "direct_read_revalidated" | "save_reserved" | "save_installed"
                        | "retained_versions_verified" | "final_write_verified"
                        | "write_statuses_verified" | "write_cleanup_pending" | "write_ready")
                    {
                        output.push_str(&format!(
                            "\nprivate {kind} timing sourceStep={step} elapsedMs={ms}",
                        ));
                    }
                }
            }
        }
    }
    for path in [
        "/var/lib/cos/clawd/audit.jsonl",
        "/run/display-home/.local/share/cos/logs/audit.jsonl",
    ] {
        if let Ok(mut file) = std::fs::File::open(path) {
            let size = file.metadata().map(|value| value.len()).unwrap_or(0);
            if file.seek(SeekFrom::Start(size.saturating_sub(8192))).is_ok() {
                let mut bytes = Vec::new();
                if file.take(8192).read_to_end(&mut bytes).is_ok() {
                    output.push_str(&format!("\n{path}:\n{}", String::from_utf8_lossy(&bytes)));
                }
            }
        }
    }
    output
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
            "GUI marker missing: {}{}",
            path.display(), private_audit_tail()
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
            layer_shell: true,
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

async fn run_home_metadata_case(group: &std::fs::File, case: usize) {
    if case == 23 {
        for ending in ["ancestor-permission", "source-rename"] {
            run_home_metadata_window(group, case, ending).await;
        }
    } else {
        run_home_metadata_window(group, case, "owner-revoke").await;
    }
}

async fn run_home_metadata_window(group: &std::fs::File, case: usize, ending: &str) {
    let home = Path::new("/run/display-home");
    let subroot = case == 23;
    let source = if subroot { home.join("Projects/review") } else { home.to_path_buf() };
    let prefix = if subroot { "subroot-metadata" } else { "metadata" };
    let scope = Scope::path(format!("{}/**", source.to_str().unwrap()));
    let data = home.join(".local/share/cos/apps/gui-boundary-test");
    if subroot {
        for name in ["ready", "statfs.json", "pending.json", "read-ready"] {
            let marker = data.join(format!("{prefix}-{name}"));
            match std::fs::remove_file(marker) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("stale subroot fixture marker: {error}"),
            }
        }
        if home.join("Projects").exists() {
            std::fs::remove_dir_all(home.join("Projects")).unwrap();
        }
    }
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
        std::fs::create_dir_all(source.join(".ssh")).unwrap();
        std::fs::create_dir_all(source.join(".config/cos")).unwrap();
        std::fs::write(source.join(".ssh/secret"), b"synthetic credential: never expose").unwrap();
        std::fs::write(source.join("metadata-visible"), b"metadata is not content\0\xff").unwrap();
        std::os::unix::fs::symlink(".ssh/secret", source.join("metadata-symlink")).unwrap();
        std::fs::hard_link(source.join(".ssh/secret"), source.join("metadata-hardlink")).unwrap();
        if subroot {
            std::fs::write(home.join("metadata-visible"), b"outside subroot: never selected").unwrap();
            std::fs::write(home.join("Projects/metadata-outside"), b"outside declared root").unwrap();
        }
    }
    let (control, child_control) = bootstrap::pair().unwrap();
    let mut pam = spawn_pam("gui", &case.to_string(), group, Stdio::from(child_control));
    assert_eq!(status(&control, &pam, Instant::now() + Duration::from_secs(30)), *b"AUTH");
    command(&control, b"CASE");
    command(&control, if subroot { b"23.." } else { b"19.." });
    let child = bootstrap::receive(control.as_fd(), *b"CHLD", Instant::now() + Duration::from_secs(5)).unwrap();
    assert_eq!(kernel::sender(child.credentials).unwrap(), ProcessIdentity::child(&pam).unwrap());
    let deadline = Instant::now() + Duration::from_secs(2);
    let launcher = loop {
        let process = ProcessIdentity::from_pidfd(child.descriptor.as_fd()).unwrap();
        if process.uid() == OWNER && process.gid() == OWNER { break process; }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    let parent = launch_parent(&launcher, case);
    command(&control, b"RUN!");
    let marker = data.join(format!("{prefix}-ready"));
    let deadline = Instant::now() + Duration::from_secs(20);
    let request = loop {
        if let Some(request) = crate::approvals::list_pending_for_owner(Some(OWNER)).into_iter().find(|request| {
            request.verb == "fs.meta" && request.scope == scope
                && request.session.starts_with("app-") && request.operation_digest.is_some()
        }) { break request; }
        assert!(pam.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline, "this GUI window never requested ordinary protected metadata consent");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_ne!(request.session, parent, "inherited parent permissions are not per-window consent");
    assert!(!marker.exists(), "App code ran before window-bound owner consent");
    assert!(crate::approvals::approve_for_owner(&request.id, crate::approvals::GrantDuration::Once,
        Some("uid:62051".into()), None, Some(OWNER + 1)).is_err());
    crate::approvals::approve_for_owner(&request.id, crate::approvals::GrantDuration::Once,
        Some("uid:62050".into()), None, Some(OWNER)).unwrap();
    wait_marker(&marker, &mut pam);
    let capacity: serde_json::Value = serde_json::from_slice(
        &std::fs::read(data.join(format!("{prefix}-statfs.json"))).unwrap()).unwrap();
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
        let source = std::fs::File::open(&source).unwrap();
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::fstatvfs(source.as_raw_fd(), &mut stats) }, 0);
        assert_eq!(capacity, serde_json::json!({
            "blocks":stats.f_blocks, "bsize":stats.f_bsize,
            "frsize":stats.f_frsize, "namelen":stats.f_namemax,
        }), "native metadata capacity must describe the retained owner backing filesystem");
    }
    assert!(matches!(crate::approvals::status_for_owner(&request.id, Some(OWNER)),
        crate::approvals::RequestStatus::Consumed));
    let retained = retained_instance(&request.session);
    let window = claw_display_control::Epoch(retained.instance.0).label();
    let startup = if subroot {
        Some(crate::paths::RoutedPathContext::for_owner(OWNER, home.to_path_buf()).scope_sync(|| {
            let gui = crate::proc::session_info_by_id(&request.session).unwrap().caps.unwrap();
            let parent = crate::proc::session_info_by_id(&parent).unwrap().caps.unwrap();
            let read = Cap::new(Verb::FS_READ, scope.clone());
            assert!(!gui.covers(&read) && !parent.covers(&read),
                "subroot read eligibility must not require startup source-content authority");
            gui
        }))
    } else {
        None
    };
    if subroot {
        let pending_marker = data.join("subroot-metadata-pending.json");
        wait_marker(&pending_marker, &mut pam);
        let pending: serde_json::Value = serde_json::from_slice(&std::fs::read(pending_marker).unwrap()).unwrap();
        assert_eq!(pending["target_display"], source.join("metadata-visible").to_str().unwrap());
        let read = crate::approvals::list_pending_for_owner(Some(OWNER)).into_iter().find(|read| {
            read.verb == "fs.read" && read.session.starts_with(&format!("gui-read-{window}-"))
        }).expect("subroot selection did not create its ordinary exact-owner review");
        assert_eq!(read.scope, scope, "selected candidate lost its authenticated Home-relative prefix");
        assert_ne!(read.operation_digest, request.operation_digest);
        assert!(!data.join("subroot-metadata-read-ready").exists());
        crate::approvals::approve_for_owner(&read.id, crate::approvals::GrantDuration::Once,
            Some("uid:62050".into()), None, Some(OWNER)).unwrap();
        wait_marker(&data.join("subroot-metadata-read-ready"), &mut pam);
        let held: serde_json::Value = serde_json::from_slice(
            &std::fs::read(data.join("subroot-metadata-read-ready")).unwrap()).unwrap();
        assert_eq!(held["directory_fd"], true);
        assert_eq!(held["directory_iterator"], true);
        assert_eq!(held["saved_entries"], true);
        assert_eq!(held["file_fd"], true);
        assert!(held["read_remaining_ms"].as_u64().unwrap() > 5_000,
            "source invalidation must start with an independently live selected-read deadline");
        crate::paths::RoutedPathContext::for_owner(OWNER, home.to_path_buf()).scope_sync(|| {
            assert_eq!(crate::proc::session_info_by_id(&request.session).unwrap().caps.as_ref(), startup.as_ref(),
                "selected read changed immutable startup authority");
        });
        assert!(matches!(crate::approvals::status_for_owner(&read.id, Some(OWNER)),
            crate::approvals::RequestStatus::Consumed));
    }
    let runtime = Path::new("/run/cos/gui").join(window);
    let metadata_path = runtime.join("home-metadata");
    assert!(std::fs::read_to_string("/proc/self/mountinfo").unwrap().lines().any(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        fields.get(4).copied() == metadata_path.to_str()
            && fields.iter().position(|field| *field == "-").is_some_and(|at|
                fields.get(at + 1) == Some(&"fuse")
                    && fields.get(at + 2).is_some_and(|source| source.starts_with("claw-native-fs-")))
    }));
    let group_path = std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd())).unwrap();
    let processes = std::fs::read_to_string(group_path.join("cgroup.procs")).unwrap().lines().map(|pid| {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.parse::<i32>().unwrap(), 0) };
        assert!(fd >= 0);
        unsafe { OwnedFd::from_raw_fd(fd as i32) }
    }).collect::<Vec<_>>();
    assert!(processes.len() >= 3, "metadata probe must retain its separate-process-group descendant");
    if subroot {
        let changed = home.join("Projects");
        let restored = home.join("Projects-source-renamed");
        match ending {
            "ancestor-permission" =>
                std::fs::set_permissions(&changed, std::fs::Permissions::from_mode(0o000)).unwrap(),
            "source-rename" => std::fs::rename(&changed, &restored).unwrap(),
            _ => panic!("unexpected source-invalidation ending"),
        }
        // Source invalidation alone must retire the instance without another revoke.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let dead = processes.iter().all(|process|
                claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap());
            if dead && !metadata_path.exists() && !runtime.exists() { break; }
            assert!(Instant::now() < deadline,
                "source-only {ending} invalidation failed to retire held FDs/processes/provider");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        match ending {
            "ancestor-permission" =>
                std::fs::set_permissions(&changed, std::fs::Permissions::from_mode(0o700)).unwrap(),
            "source-rename" => std::fs::rename(&restored, &changed).unwrap(),
            _ => unreachable!(),
        }
        assert!(processes.iter().all(|process|
            claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap()),
            "restoring the source revived a retired window");
    } else {
        let revoked = crate::clawd::client::request("/run/cos/clawd.sock", crate::clawd::wire::Request::new(
            crate::clawd::routes::Command::PermissionRevoke,
            serde_json::json!({"owner_uid":OWNER,"session":request.session}),
        )).await.unwrap();
        assert!(revoked.ok, "ordinary owner-window revocation did not complete: {:?}", revoked.error);
        for process in &processes {
            assert!(claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap());
        }
    }
    assert!(!metadata_path.exists() && !runtime.exists(), "Home provider/runtime cleanup was not acknowledged");
    assert_eq!(std::fs::read(source.join("metadata-visible")).unwrap(), b"metadata is not content\0\xff");
    assert!(!source.join("metadata-new-file").exists());
    if subroot {
        assert_eq!(std::fs::read(home.join("metadata-visible")).unwrap(), b"outside subroot: never selected");
        assert_eq!(std::fs::read(home.join("Projects/metadata-outside")).unwrap(), b"outside declared root");
    }
    command(&control, b"WAIT");
    assert_eq!(status(&control, &pam, Instant::now() + Duration::from_secs(30)), *b"FAIL");
    command(&control, b"CLSE");
    assert!(pam.wait().unwrap().success());
    println!("private GUI case {case}/{ending}: owner-window metadata root, exact selected-node scope and checked provider retirement passed");
}

async fn run_selected_read_case(group: &std::fs::File) {
    for inherited in [false, true] {
        run_selected_read_window(group, inherited).await;
    }
    println!("private GUI case 20: protected owner-window selected binary read, independent optional authority and checked revoke passed");
}

async fn run_selected_read_window(group: &std::fs::File, inherited: bool) {
    use clawd_client::system_review::{
        PermissionChoice, PermissionSelection, ReviewAction, ReviewDecision, SystemReview,
    };
    use std::os::unix::ffi::OsStringExt;

    let home = Path::new("/run/display-home");
    let data = home.join(".local/share/cos/apps/gui-boundary-test");
    let target = home.join("read-selected").join(std::ffi::OsString::from_vec(b"binary-\xff.bin".to_vec()));
    let bytes = (0_usize..2 * 1024 * 1024 + 29)
        .map(|index| index.wrapping_mul(43).wrapping_add(17) as u8).collect::<Vec<_>>();
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
        std::fs::create_dir_all(home.join("read-selected")).unwrap();
        std::fs::create_dir_all(home.join("read-delegated")).unwrap();
        std::fs::write(&target, &bytes).unwrap();
        std::fs::write(home.join("read-selected/sibling"), b"unselected sibling content").unwrap();
        std::fs::write(home.join("read-delegated/not-selected"), b"capability is not selection").unwrap();
        std::fs::write(home.join("read-unselected"), b"unselected owner content").unwrap();
        for marker in ["selected-read-pending.json", "selected-read-ungranted.json", "selected-read-ready",
            "selected-read-unfinished.json", "selected-read-timing.jsonl"] {
            let marker = data.join(marker);
            if marker.exists() { std::fs::remove_file(marker).unwrap(); }
        }
    }
    let (control, child_control) = bootstrap::pair().unwrap();
    let mut pam = spawn_pam("gui", "20", group, Stdio::from(child_control));
    assert_eq!(status(&control, &pam, Instant::now() + Duration::from_secs(30)), *b"AUTH");
    command(&control, b"CASE");
    command(&control, b"20..");
    let child = bootstrap::receive(control.as_fd(), *b"CHLD", Instant::now() + Duration::from_secs(5)).unwrap();
    assert_eq!(kernel::sender(child.credentials).unwrap(), ProcessIdentity::child(&pam).unwrap());
    let deadline = Instant::now() + Duration::from_secs(2);
    let launcher = loop {
        let process = ProcessIdentity::from_pidfd(child.descriptor.as_fd()).unwrap();
        if process.uid() == OWNER && process.gid() == OWNER { break process; }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    let parent = launch_parent(&launcher, 20);
    if inherited {
        crate::paths::RoutedPathContext::for_owner(OWNER, home.to_path_buf()).scope_sync(|| {
            let mut session = crate::proc::session_info_by_id(&parent).unwrap();
            let caps = session.caps.as_mut().unwrap();
            for verb in [Verb::FS_READ, Verb::FS_WRITE, Verb::FS_WATCH, Verb::FS_DELETE] {
                caps.insert(Cap::new(verb, Scope::path("/run/display-home/**")));
            }
            crate::proc::register_session_for_owner(session, OWNER).unwrap();
        });
    }
    command(&control, b"RUN!");
    let deadline = Instant::now() + Duration::from_secs(20);
    let metadata = loop {
        if let Some(request) = crate::approvals::list_pending_for_owner(Some(OWNER)).into_iter().find(|request| {
            request.verb == "fs.meta" && request.scope == Scope::path("/run/display-home/**")
                && request.session.starts_with("app-") && request.operation_digest.is_some()
        }) { break request; }
        assert!(pam.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline, "selected-read window did not request separate metadata consent");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_ne!(metadata.session, parent);
    assert!(!data.join("selected-read-pending.json").exists(), "App ran before metadata consent");
    crate::approvals::approve_for_owner(&metadata.id, crate::approvals::GrantDuration::Once,
        Some("uid:62050".into()), None, Some(OWNER)).unwrap();
    wait_marker(&data.join("selected-read-pending.json"), &mut pam);
    let ungranted: Vec<usize> = serde_json::from_slice(
        &std::fs::read(data.join("selected-read-ungranted.json")).unwrap()).unwrap();
    assert_eq!(ungranted, if inherited { vec![] } else { vec![0, 1, 2, 3] });
    let pending: claw_os_sdk::generated::GuiFileRead =
        serde_json::from_slice(&std::fs::read(data.join("selected-read-pending.json")).unwrap()).unwrap();
    assert_eq!(pending.state, "pending");
    let read = crate::approvals::list_pending_for_owner(Some(OWNER)).into_iter()
        .find(|request| Some(&request.id) == pending.approval_request_id.as_ref()).unwrap();
    assert_eq!(read.verb, "fs.read");
    assert_eq!(read.scope, Scope::path("/run/display-home/**"));
    assert!(read.session.starts_with("gui-read-"));
    assert_ne!(read.session, metadata.session);
    assert!(read.operation_digest.is_some());
    assert!(!data.join("selected-read-ready").exists());
    let retained = retained_instance(&metadata.session);
    let original_caps = crate::paths::RoutedPathContext::for_owner(OWNER, home.to_path_buf()).scope_sync(|| {
        let gui = crate::proc::session_info_by_id(&metadata.session).unwrap();
        let parent = crate::proc::session_info_by_id(&parent).unwrap();
        (gui.caps.unwrap(), parent.caps.unwrap())
    });
    let window = claw_display_control::Epoch(retained.instance.0).label();
    assert!(read.session.starts_with(&format!("gui-read-{window}-")));
    let mut review_command = ProcessCommand::new("/usr/local/bin/cos");
    review_command.args(["review", "show", &read.id]).env_clear()
        .env("HOME", home).env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .current_dir(home).stdin(Stdio::null());
    unsafe {
        review_command.pre_exec(|| {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(OWNER) != 0 || libc::setuid(OWNER) != 0
            { return Err(std::io::Error::last_os_error()); }
            Ok(())
        });
    }
    let mut review_command = tokio::process::Command::from(review_command);
    review_command.kill_on_drop(true);
    let shown = tokio::time::timeout(Duration::from_secs(15), review_command.output()).await.unwrap().unwrap();
    assert!(shown.status.success(), "{}", String::from_utf8_lossy(&shown.stderr));
    let review: SystemReview = serde_json::from_slice(&shown.stdout).unwrap();
    review.validate().unwrap();
    assert!(review.initiator.contains("gui-boundary-test"));
    assert_eq!(review.permissions.len(), 1);
    assert_eq!(review.permissions[0].verb, "fs.read");
    assert!(review.context.iter().any(|detail| detail.value.contains("binary-\\xff.bin")
        && detail.value.contains(&window) && detail.value.contains("Read ONLY selected file")));
    let decision = ReviewDecision {
        id: review.id.clone(), revision: review.revision, action: ReviewAction::ApplyChoices,
        choices: vec![PermissionSelection {
            permission_id: review.permissions[0].id.clone(), choice: PermissionChoice::AllowOnce,
        }],
    };
    decision.validate_for(&review).unwrap();
    let wrong = crate::clawd::client::request("/run/cos/clawd.sock", crate::clawd::wire::Request::new(
        crate::clawd::routes::Command::SystemReviewDecide,
        serde_json::json!({"owner_uid":OWNER + 1,"review":decision}),
    )).await.unwrap();
    assert!(!wrong.ok, "another owner approved this window's selected file");
    assert!(matches!(crate::approvals::status_for_owner(&read.id, Some(OWNER)),
        crate::approvals::RequestStatus::Pending));
    let approved = crate::clawd::client::request("/run/cos/clawd.sock", crate::clawd::wire::Request::new(
        crate::clawd::routes::Command::SystemReviewDecide,
        serde_json::json!({"owner_uid":OWNER,"review":decision}),
    )).await.unwrap();
    assert!(approved.ok, "protected exact owner decision failed: {:?}", approved.error);
    wait_marker(&data.join("selected-read-ready"), &mut pam);
    assert!(matches!(crate::approvals::status_for_owner(&read.id, Some(OWNER)),
        crate::approvals::RequestStatus::Consumed));
    let repeated = crate::clawd::client::request("/run/cos/clawd.sock", crate::clawd::wire::Request::new(
        crate::clawd::routes::Command::SystemReviewDecide,
        serde_json::json!({"owner_uid":OWNER,"review":decision}),
    )).await.unwrap();
    assert!(!repeated.ok, "consumed selection decision was reusable");
    let selected_cap = Cap::new(Verb::FS_READ, Scope::path("/run/display-home/**"));
    crate::paths::RoutedPathContext::for_owner(OWNER, home.to_path_buf()).scope_sync(|| {
        let gui = crate::proc::session_info_by_id(&metadata.session).unwrap();
        let parent = crate::proc::session_info_by_id(&parent).unwrap();
        assert_eq!(gui.caps.as_ref().unwrap(), &original_caps.0, "selection changed immutable GUI caps");
        assert_eq!(parent.caps.as_ref().unwrap(), &original_caps.1, "selection changed parent authority");
        assert_eq!(gui.caps.unwrap().covers(&selected_cap), inherited);
        assert_eq!(parent.caps.unwrap().covers(&selected_cap), inherited);
    });
        let unfinished: Vec<claw_os_sdk::generated::GuiFileRead> = serde_json::from_slice(
            &std::fs::read(data.join("selected-read-unfinished.json")).unwrap()).unwrap();
        assert_eq!(unfinished.len(), 2);
        let pending_id = unfinished[0].approval_request_id.as_ref().unwrap();
        let unused_id = unfinished[1].approval_request_id.as_ref().unwrap();
        crate::approvals::approve_for_owner(unused_id, crate::approvals::GrantDuration::Once,
            Some("uid:62050".into()), None, Some(OWNER)).unwrap();
        assert!(matches!(crate::approvals::status_for_owner(unused_id, Some(OWNER)),
            crate::approvals::RequestStatus::Approved));
    let runtime = Path::new("/run/cos/gui").join(window);
    let mountpoint = runtime.join("home-metadata");
    let group_path = std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd())).unwrap();
    let processes = std::fs::read_to_string(group_path.join("cgroup.procs")).unwrap().lines().map(|pid| {
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.parse::<i32>().unwrap(), 0) };
        assert!(descriptor >= 0);
        unsafe { OwnedFd::from_raw_fd(descriptor as i32) }
    }).collect::<Vec<_>>();
    assert!(processes.len() >= 3, "selected read did not retain its separately grouped descendant");
    let metadata_generation = crate::approvals::generations::current(Some(OWNER), &metadata.session).unwrap();
    let revoked = crate::clawd::client::request("/run/cos/clawd.sock", crate::clawd::wire::Request::new(
        crate::clawd::routes::Command::PermissionRevoke,
        serde_json::json!({"owner_uid":OWNER,"session":read.session}),
    )).await.unwrap();
    assert!(revoked.ok, "independent read revocation did not complete: {:?}", revoked.error);
    assert_eq!(crate::approvals::generations::current(Some(OWNER), &metadata.session).unwrap(),
        metadata_generation, "read revocation must not replace the metadata decision");
    assert!(matches!(crate::approvals::status_for_owner(pending_id, Some(OWNER)),
        crate::approvals::RequestStatus::Denied));
    assert!(matches!(crate::approvals::status_for_owner(unused_id, Some(OWNER)),
        crate::approvals::RequestStatus::Consumed));
    let recent = crate::approvals::list_recent_for_owner(1000, Some(OWNER));
    assert!(recent.iter().any(|entry| &entry.request.id == pending_id
        && entry.decision.decided_by.as_deref() == Some("system:validation")));
    assert!(recent.iter().any(|entry| &entry.request.id == unused_id
        && entry.decision.outcome == crate::approvals::Outcome::Approved
        && entry.decision.decided_by.as_deref() == Some("uid:62050")));
    for process in processes {
        assert!(claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap());
    }
    assert!(std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd())).unwrap()
        .as_os_str().as_encoded_bytes().ends_with(b" (deleted)"));
    assert!(!mountpoint.exists() && !runtime.exists(), "selected-read provider/runtime retirement was not checked");
    assert!(!std::fs::read_to_string("/proc/self/mountinfo").unwrap().lines().any(|line|
        line.split_whitespace().nth(4) == mountpoint.to_str()));
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
    assert_eq!(std::fs::read(home.join("read-selected/sibling")).unwrap(), b"unselected sibling content");
    assert_eq!(std::fs::read(home.join("read-delegated/not-selected")).unwrap(), b"capability is not selection");
    command(&control, b"WAIT");
    assert_eq!(status(&control, &pam, Instant::now() + Duration::from_secs(30)), *b"FAIL");
    command(&control, b"CLSE");
    assert!(pam.wait().unwrap().success());
    println!("private GUI selected read authority {}: passed",
        if inherited { "already inherited, never implicitly selected" } else { "separate post-start optional grant" });
}

async fn approve_native_resource(id: &str) -> crate::approvals::Request {
    assert!(crate::approvals::lookup_pending(id).unwrap().operation_digest.is_some());
    approve_protected_owner_request(id).await
}

async fn approve_protected_owner_request(id: &str) -> crate::approvals::Request {
    use clawd_client::system_review::{PermissionChoice, PermissionSelection, ReviewAction, ReviewDecision, SystemReview};
    let request = crate::approvals::lookup_pending(id).unwrap();
    assert_eq!(request.owner_uid, Some(OWNER));
    let mut command = ProcessCommand::new("/usr/local/bin/cos");
    command.args(["review", "show", id]).env_clear().env("HOME", "/run/display-home")
        .env("PATH", "/usr/local/bin:/usr/bin:/bin").current_dir("/run/display-home").stdin(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setgroups(0, std::ptr::null()) != 0 || libc::setgid(OWNER) != 0 || libc::setuid(OWNER) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let shown = tokio::time::timeout(Duration::from_secs(15), command.output()).await.unwrap().unwrap();
    assert!(shown.status.success(), "{}", String::from_utf8_lossy(&shown.stderr));
    let review: SystemReview = serde_json::from_slice(&shown.stdout).unwrap();
    review.validate().unwrap();
    let decision = ReviewDecision {
        id: review.id.clone(), revision: review.revision, action: ReviewAction::ApplyChoices,
        choices: vec![PermissionSelection {
            permission_id: review.permissions[0].id.clone(), choice: PermissionChoice::AllowOnce,
        }],
    };
    decision.validate_for(&review).unwrap();
    let wrong = crate::clawd::client::request("/run/cos/clawd.sock", crate::clawd::wire::Request::new(
        crate::clawd::routes::Command::SystemReviewDecide,
        serde_json::json!({"owner_uid":OWNER + 1,"review":decision}),
    )).await.unwrap();
    assert!(!wrong.ok);
    let approved = crate::clawd::client::request("/run/cos/clawd.sock", crate::clawd::wire::Request::new(
        crate::clawd::routes::Command::SystemReviewDecide,
        serde_json::json!({"owner_uid":OWNER,"review":decision}),
    )).await.unwrap();
    assert!(approved.ok, "ordinary protected resource decision failed: {:?}", approved.error);
    request
}

async fn run_selected_write_case(group: &std::fs::File) {
    use std::os::unix::ffi::OsStringExt;
    let home = Path::new("/run/display-home");
    let data = home.join(".local/share/cos/apps/gui-boundary-test");
    let initial: Vec<u8> = (0_usize..4113).map(|index| index.wrapping_mul(7).wrapping_add(1) as u8).collect();
    let non_utf = home.join(std::ffi::OsString::from_vec(b"write-\xff.bin".to_vec()));
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
        std::fs::write(home.join("write-shared"), &initial).unwrap();
        std::fs::write(home.join("write-only"), b"write-only-before\0\xfe").unwrap();
        std::fs::write(home.join("write-sibling"), b"unselected write sibling").unwrap();
        std::fs::write(home.join("write-pending-sibling"), b"pending sibling").unwrap();
        std::fs::write(&non_utf, b"non-UTF8 inverse stays unsupported").unwrap();
    }
    let (control, child_control) = bootstrap::pair().unwrap();
    let mut pam = spawn_pam("gui", "21", group, Stdio::from(child_control));
    assert_eq!(status(&control, &pam, Instant::now() + Duration::from_secs(30)), *b"AUTH");
    command(&control, b"CASE");
    command(&control, b"21..");
    let child = bootstrap::receive(control.as_fd(), *b"CHLD", Instant::now() + Duration::from_secs(5)).unwrap();
    assert_eq!(kernel::sender(child.credentials).unwrap(), ProcessIdentity::child(&pam).unwrap());
    let deadline = Instant::now() + Duration::from_secs(2);
    let launcher = loop {
        let process = ProcessIdentity::from_pidfd(child.descriptor.as_fd()).unwrap();
        if process.uid() == OWNER && process.gid() == OWNER { break process; }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    let parent = launch_parent(&launcher, 21);
    command(&control, b"RUN!");
    let deadline = Instant::now() + Duration::from_secs(20);
    let metadata = loop {
        if let Some(request) = crate::approvals::list_pending_for_owner(Some(OWNER)).into_iter().find(|request|
            request.verb == "fs.meta" && request.scope == Scope::path("/run/display-home/**")
                && request.session.starts_with("app-") && request.operation_digest.is_some())
        { break request; }
        assert!(pam.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    crate::approvals::approve_for_owner(&metadata.id, crate::approvals::GrantDuration::Once,
        Some("uid:62050".into()), None, Some(OWNER)).unwrap();
    wait_marker(&data.join("write-read-pending.json"), &mut pam);
    let read: claw_os_sdk::generated::GuiFileRead =
        serde_json::from_slice(&std::fs::read(data.join("write-read-pending.json")).unwrap()).unwrap();
    let read_request = approve_native_resource(read.approval_request_id.as_ref().unwrap()).await;
    wait_marker(&data.join("write-pending.json"), &mut pam);
    let pending: serde_json::Value = serde_json::from_slice(&std::fs::read(data.join("write-pending.json")).unwrap()).unwrap();
    let write_only: claw_os_sdk::generated::GuiFileWrite = serde_json::from_value(pending["write_only"].clone()).unwrap();
    let shared: claw_os_sdk::generated::GuiFileWrite = serde_json::from_value(pending["shared"].clone()).unwrap();
    let only_request = approve_native_resource(write_only.approval_request_id.as_ref().unwrap()).await;
    let write_request = approve_native_resource(shared.approval_request_id.as_ref().unwrap()).await;
    assert_eq!(only_request.verb, "fs.write");
    assert_eq!(write_request.verb, "fs.write");
    assert_ne!(write_request.session, read_request.session);
    wait_marker(&data.join("selected-write-ready"), &mut pam);
    let retained = retained_instance(&metadata.session);
    let runtime = Path::new("/run/cos/gui").join(claw_display_control::Epoch(retained.instance.0).label());
    let context = crate::paths::RoutedPathContext::for_owner(OWNER, home.to_path_buf());
    let sid = crate::clawd::gui::selected_mutation_session(OWNER, &metadata.session).unwrap();
    let mutations = context.clone().scope_sync(|| crate::session::iter_mutations(&sid)).unwrap();
    let previous = mutations.iter().find_map(|record| match &record.mutation {
        crate::session::Mutation::FsWrite { path, prev_blob: Some(blob) }
            if Path::new(path) == home.join("write-only") => Some(blob),
        _ => None,
    }).unwrap();
    assert_eq!(context.clone().scope_sync(|| crate::session::read_blob(&sid, previous)).unwrap(), b"write-only-before\0\xfe");
    let projection = context.clone().scope_sync(|| crate::session::journal::projection::build(
        &crate::session::journal::Partition::Session(sid.clone()), OWNER)).unwrap();
    assert!(projection.health.is_verified());
    assert!(projection.mutations.len() >= 4);
    assert!(projection.mutations.iter().all(|effect| effect.status == "committed"
        && effect.session_mutation.is_some_and(|seq| mutations.iter().any(|record| record.seq == seq))));
    assert!(projection.mutations.iter().any(|effect| effect.route == "system.file.replace"));
    context.clone().scope_sync(|| {
        assert!(crate::session::get_caps(&sid).unwrap().is_empty(), "evidence session became authority");
        let parent = crate::proc::session_info_by_id(&parent).unwrap();
        let gui = crate::proc::session_info_by_id(&metadata.session).unwrap();
        for verb in [Verb::FS_READ, Verb::FS_WRITE] {
            let cap = Cap::new(verb, Scope::path("/run/display-home/**"));
            assert!(!parent.caps.as_ref().unwrap().covers(&cap));
            assert!(!gui.caps.as_ref().unwrap().covers(&cap));
        }
    });
    let unfinished: serde_json::Value = serde_json::from_slice(
        &std::fs::read(data.join("write-cleanup-reviews.json")).unwrap()).unwrap();
    let pending_id = unfinished["pending"]["approval_request_id"].as_str().unwrap();
    let unused_id = unfinished["unused"]["approval_request_id"].as_str().unwrap();
    approve_native_resource(unused_id).await;
    let group_path = std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd())).unwrap();
    let processes: Vec<OwnedFd> = std::fs::read_to_string(group_path.join("cgroup.procs")).unwrap().lines().map(|pid| {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.parse::<i32>().unwrap(), 0) };
        assert!(fd >= 0);
        unsafe { OwnedFd::from_raw_fd(fd as i32) }
    }).collect();
    assert!(processes.len() >= 3);
    let metadata_generation = crate::approvals::generations::current(Some(OWNER), &metadata.session).unwrap();
    let revoked = crate::clawd::client::request("/run/cos/clawd.sock", crate::clawd::wire::Request::new(
        crate::clawd::routes::Command::PermissionRevoke,
        serde_json::json!({"owner_uid":OWNER,"session":write_request.session}),
    )).await.unwrap();
    assert!(revoked.ok, "write revocation did not check complete retirement: {:?}", revoked.error);
    assert_eq!(crate::approvals::generations::current(Some(OWNER), &metadata.session).unwrap(), metadata_generation);
    for process in processes { assert!(claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap()); }
    assert!(!runtime.exists());
    assert!(matches!(crate::approvals::status_for_owner(pending_id, Some(OWNER)), crate::approvals::RequestStatus::Denied));
    assert!(matches!(crate::approvals::status_for_owner(unused_id, Some(OWNER)), crate::approvals::RequestStatus::Consumed));
    let recent = crate::approvals::list_recent_for_owner(1000, Some(OWNER));
    assert!(recent.iter().any(|entry| entry.request.id == pending_id
        && entry.decision.decided_by.as_deref() == Some("system:validation")));
    assert!(recent.iter().any(|entry| entry.request.id == unused_id
        && entry.decision.outcome == crate::approvals::Outcome::Approved));
    assert!(context.clone().scope_sync(|| crate::session::get_meta(&sid)).unwrap().status == crate::session::Status::Done);
    let mut expected: Vec<u8> = (0_usize..12317).map(|index| index.wrapping_mul(53).wrapping_add(9) as u8).collect();
    expected[0] = b'Z';
    assert_eq!(std::fs::read(home.join("write-shared")).unwrap(), expected);
    assert_eq!(std::fs::read(home.join("write-only")).unwrap(),
        (0_usize..8199).map(|index| index.wrapping_mul(31).wrapping_add(3) as u8).collect::<Vec<_>>());
    assert_eq!(std::fs::read(home.join("write-sibling")).unwrap(), b"unselected write sibling");
    assert_eq!(std::fs::read(non_utf).unwrap(), b"non-UTF8 inverse stays unsupported");
    assert!(!home.join("unreserved-save-sibling").exists());
    command(&control, b"WAIT");
    assert_eq!(status(&control, &pam, Instant::now() + Duration::from_secs(30)), *b"FAIL");
    command(&control, b"CLSE");
    assert!(pam.wait().unwrap().success());
    println!("private GUI case 21: independent selected writes, canonical atomic save, preserved read revisions and checked retirement passed");
}

async fn run_selected_create_case(group: &std::fs::File) {
    let home = Path::new("/run/display-home");
    let source = home.join("Projects");
    let moved = home.join("Projects-created-moved");
    let data = home.join(".local/share/cos/apps/gui-boundary-test");
    let leaf = "new ; '$draft'.bin";
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
        if source.exists() { std::fs::remove_dir_all(&source).unwrap(); }
        std::fs::create_dir(&source).unwrap();
        for marker in ["create-pending.json","create-slot.json","create-go","create-race-pending.json",
            "create-race-slot.json","create-race-go","create-cleanup-reviews.json","selected-create-ready"]
        {
            let path = data.join(marker);
            if path.exists() { std::fs::remove_file(path).unwrap(); }
        }
    }
    let (control, child_control) = bootstrap::pair().unwrap();
    let mut pam = spawn_pam("gui", "24", group, Stdio::from(child_control));
    assert_eq!(status(&control, &pam, Instant::now() + Duration::from_secs(30)), *b"AUTH");
    command(&control, b"CASE");
    command(&control, b"24..");
    let child = bootstrap::receive(control.as_fd(), *b"CHLD", Instant::now() + Duration::from_secs(5)).unwrap();
    assert_eq!(kernel::sender(child.credentials).unwrap(), ProcessIdentity::child(&pam).unwrap());
    let deadline = Instant::now() + Duration::from_secs(2);
    let launcher = loop {
        let process = ProcessIdentity::from_pidfd(child.descriptor.as_fd()).unwrap();
        if process.uid() == OWNER && process.gid() == OWNER { break process; }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    let parent_session = launch_parent(&launcher, 24);
    command(&control, b"RUN!");
    let deadline = Instant::now() + Duration::from_secs(20);
    let metadata = loop {
        if let Some(request) = crate::approvals::list_pending_for_owner(Some(OWNER)).into_iter().find(|request|
            request.verb == "fs.meta" && request.scope == Scope::path("/run/display-home/Projects/**")
                && request.session.starts_with("app-") && request.operation_digest.is_some())
        { break request; }
        assert!(pam.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(!source.join(leaf).exists());
    crate::approvals::approve_for_owner(&metadata.id, crate::approvals::GrantDuration::Once,
        Some("uid:62050".into()), None, Some(OWNER)).unwrap();
    wait_marker(&data.join("create-pending.json"), &mut pam);
    let create: claw_os_sdk::generated::GuiFileCreate =
        serde_json::from_slice(&std::fs::read(data.join("create-pending.json")).unwrap()).unwrap();
    assert_eq!(create.parent_node, 1);
    assert_eq!(create.state, "pending");
    assert!(!source.join(leaf).exists());
    let decision = approve_native_resource(create.approval_request_id.as_ref().unwrap()).await;
    assert_eq!(decision.verb, "fs.write");
    assert!(decision.operation_digest.is_some());
    wait_marker(&data.join("create-slot.json"), &mut pam);
    let slot: claw_os_sdk::generated::GuiFileCreateSlot =
        serde_json::from_slice(&std::fs::read(data.join("create-slot.json")).unwrap()).unwrap();
    assert_eq!(slot.target_name, leaf);
    assert!(!source.join(leaf).exists() && !source.join(&slot.name).exists());
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
        std::fs::write(data.join("create-go"), b"go").unwrap();
    }
    wait_marker(&data.join("create-race-pending.json"), &mut pam);
    let race: claw_os_sdk::generated::GuiFileCreate =
        serde_json::from_slice(&std::fs::read(data.join("create-race-pending.json")).unwrap()).unwrap();
    let _race_decision = approve_native_resource(race.approval_request_id.as_ref().unwrap()).await;
    wait_marker(&data.join("create-race-slot.json"), &mut pam);
    let expected: Vec<u8> = (0_usize..16387).map(|index| index.wrapping_mul(47).wrapping_add(5) as u8).collect();
    assert_eq!(std::fs::read(source.join(leaf)).unwrap(), expected);
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
        std::fs::write(source.join("collision"), b"foreign creation must survive\0\xfe").unwrap();
        std::fs::write(data.join("create-race-go"), b"go").unwrap();
    }
    wait_marker(&data.join("selected-create-ready"), &mut pam);
    assert_eq!(std::fs::read(source.join("collision")).unwrap(), b"foreign creation must survive\0\xfe");
    assert!(!source.join("unselected-child").exists() && !home.join(leaf).exists());
    let retained = retained_instance(&metadata.session);
    let runtime = Path::new("/run/cos/gui").join(claw_display_control::Epoch(retained.instance.0).label());
    let context = crate::paths::RoutedPathContext::for_owner(OWNER, home.to_path_buf());
    let sid = crate::clawd::gui::selected_mutation_session(OWNER, &metadata.session).unwrap();
    let mutations = context.clone().scope_sync(|| crate::session::iter_mutations(&sid)).unwrap();
    assert_eq!(mutations.len(), 1);
    assert!(matches!(&mutations[0].mutation, crate::session::Mutation::FsWrite { path, prev_blob: None }
        if Path::new(path) == source.join(leaf)));
    let projection = context.clone().scope_sync(|| crate::session::journal::projection::build(
        &crate::session::journal::Partition::Session(sid.clone()), OWNER)).unwrap();
    assert!(projection.health.is_verified());
    assert_eq!(projection.mutations.len(), 1);
    assert_eq!(projection.mutations[0].status, "committed");
    assert_eq!(projection.mutations[0].session_mutation, Some(mutations[0].seq));
    context.clone().scope_sync(|| {
        assert!(crate::session::get_caps(&sid).unwrap().is_empty());
        for session in [&metadata.session, &parent_session] {
            let caps = crate::proc::session_info_by_id(session).unwrap().caps.unwrap();
            assert!(!caps.covers(&Cap::new(Verb::FS_WRITE, Scope::path("/run/display-home/Projects/**"))));
        }
    });
    let reviews: serde_json::Value = serde_json::from_slice(
        &std::fs::read(data.join("create-cleanup-reviews.json")).unwrap()).unwrap();
    let pending_id = reviews["pending"]["approval_request_id"].as_str().unwrap();
    let unused_id = reviews["unused"]["approval_request_id"].as_str().unwrap();
    approve_native_resource(unused_id).await;
    let group_path = std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd())).unwrap();
    let processes: Vec<OwnedFd> = std::fs::read_to_string(group_path.join("cgroup.procs")).unwrap().lines().map(|pid| {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.parse::<i32>().unwrap(), 0) };
        assert!(fd >= 0);
        unsafe { OwnedFd::from_raw_fd(fd as i32) }
    }).collect();
    assert!(processes.len() >= 3);
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
        std::fs::rename(&source, &moved).unwrap();
    }
    command(&control, b"WAIT");
    assert_eq!(status(&control, &pam, Instant::now() + Duration::from_secs(30)), *b"FAIL");
    assert!(!runtime.exists());
    for process in processes { assert!(claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap()); }
    assert!(matches!(crate::approvals::status_for_owner(pending_id, Some(OWNER)), crate::approvals::RequestStatus::Denied));
    assert!(matches!(crate::approvals::status_for_owner(unused_id, Some(OWNER)), crate::approvals::RequestStatus::Consumed));
    assert_eq!(std::fs::read(moved.join(leaf)).unwrap(), expected);
    assert_eq!(std::fs::read(moved.join("collision")).unwrap(), b"foreign creation must survive\0\xfe");
    assert_eq!(context.scope_sync(|| crate::session::get_meta(&sid)).unwrap().status, crate::session::Status::Done);
    command(&control, b"CLSE");
    assert!(pam.wait().unwrap().success());
    println!("private GUI case 24: protected absent-child creation, atomic no-replace, canonical Missing inverse and checked closing cleanup passed");
}

async fn run_case(case: usize, group: &std::fs::File) {
    if case == 19 || case == 23 { return run_home_metadata_case(group, case).await; }
    if case == 20 { return run_selected_read_case(group).await; }
    if case == 21 { return run_selected_write_case(group).await; }
    if case == 24 { return run_selected_create_case(group).await; }
    let (control, child_control) = bootstrap::pair().unwrap();
    let mut pam = spawn_pam("gui", &case.to_string(), group, Stdio::from(child_control));
    assert_eq!(
        status(&control, &pam, Instant::now() + Duration::from_secs(30)),
        *b"AUTH"
    );
    println!("private GUI case {case}: authenticated PAM display");
    let host = if [8, 9, 18].contains(&case) {
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
    if [4, 6, 7, 8, 9, 10, 11, 17, 18].contains(&case) {
        match std::fs::remove_file(data.join("revoke-ready")) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("clear previous private GUI marker: {error}"),
        }
    }
    command(&control, b"RUN!");
    if [4, 6, 7, 8, 9, 10, 11, 17, 18].contains(&case) {
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
            4 | 17 => {
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
            8 | 9 | 18 => {
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
                if [9, 18].contains(&case)
                    && !claw_display_control::identity::pidfd_exited(host.as_fd()).unwrap()
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
        if [4, 17].contains(&case) {
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
        let expected = if [4, 6, 7, 8, 9, 10, 11, 17, 18].contains(&case) {
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
    if [8, 9, 18].contains(&case) {
        assert_eq!(
            pam_status.code(),
            Some(4),
            "lost supervisor must be an explicit PAM failure"
        );
    } else {
        assert!(pam_status.success());
    }
    if [2, 15].contains(&case) {
        let log = std::fs::read_to_string(format!("/run/gui-fixture/pam-{case}.log")).unwrap();
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
    private_gui_fixture(false, false);
}

#[test]
#[ignore = "requires the private authenticated Root PAM/display fixture and cosmic-session"]
fn private_session_startup_failure() {
    private_gui_fixture(true, false);
}

#[test]
#[ignore = "requires the private authenticated PAM/COSMIC/FileManager1 fixture"]
fn private_file_manager_session() {
    private_gui_fixture(false, true);
}

#[test]
#[ignore = "requires the private Root namespace, /dev/fuse, bubblewrap and writable cgroup v2"]
fn private_native_fs_kernel() {
    use std::fs::Permissions;
    use std::io::{Read, Write};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use crate::worker::native_fs::NativeFsView;
    use crate::worker::policy::{LaunchPolicy, Mount, MountClass, NetworkPolicy, SeccompProfile, StdioPlan, TrustTier};

    fn own(path: &Path) {
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::chown(path.as_ptr(), OWNER, OWNER) }, 0);
    }
    fn receive(child: &mut Child, expected: &[u8; 4]) {
        let output = child.stdout.as_mut().unwrap();
        let mut poll = libc::pollfd { fd: output.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        assert!(unsafe { libc::poll(&mut poll, 1, 15000) } > 0, "native filesystem probe timed out");
        let mut tag = [0; 4];
        output.read_exact(&mut tag).unwrap();
        assert_eq!(&tag, expected);
    }
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert_ne!(std::fs::read_link("/proc/self/ns/mnt").unwrap(), std::fs::read_link("/proc/1/ns/mnt").unwrap());
    assert_eq!(std::fs::read("/run/gui-fixture/isolated").unwrap(), b"private-v1");
    assert_eq!(std::env::var("COS_DATA_DIR").unwrap(), "/var/lib/cos");
    assert!(std::fs::read_to_string("/proc/self/mountinfo").unwrap().lines().any(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        fields.get(4) == Some(&"/var/lib") && fields.iter().position(|field| *field == "-")
            .is_some_and(|separator| fields.get(separator + 1) == Some(&"tmpfs"))
    }), "native mutation evidence must use the private bootstrap's volatile state mount");
    assert_eq!(std::fs::read_to_string("/proc/self/loginuid").unwrap().trim(), "4294967295");
    let cgroup = std::fs::read_to_string("/run/gui-fixture/cgroup").unwrap();
    assert!(cgroup.trim().starts_with("/sys/fs/cgroup/claw-display-fixture."));
    let cgroup = Path::new(cgroup.trim());
    std::fs::write(cgroup.join("cgroup.subtree_control"), b"+cpu +memory +pids").unwrap();
    let _ = tracing_subscriber::fmt().with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr).try_init();
    crate::agentd::guard::mark_broker_process();
    for ending in ["revoke", "expiry", "worker-failure", "write"] {
        let source = Path::new("/run/display-home").join(format!("native-fs-source-{ending}"));
        let data = Path::new("/run/display-home").join(format!("native-fs-data-{ending}"));
        let mountpoint = Path::new("/run/gui-public").join(format!("native-fs-view-{ending}"));
        for directory in [&source, &data, &source.join("selected-dir"), &source.join("selected-dir/deeper"),
            &source.join(".ssh"), &source.join("empty-dir")]
        {
            std::fs::create_dir(directory).unwrap();
            std::fs::set_permissions(directory, Permissions::from_mode(0o700)).unwrap();
            own(directory);
        }
        std::fs::create_dir(&mountpoint).unwrap();
        std::fs::set_permissions(&mountpoint, Permissions::from_mode(0o700)).unwrap();
        let binary: Vec<u8> = (0_usize..2 * 1024 * 1024 + 29)
            .map(|index| index.wrapping_mul(37).wrapping_add(11) as u8).collect();
        let raw = std::ffi::OsString::from_vec(b"binary-\xff".to_vec());
        for (name, bytes) in [
            (std::ffi::OsString::from("binary"), binary.as_slice()),
            ("unselected".into(), b"unselected content".as_slice()),
            (raw.clone(), b"\0\xffbinary-name\n".as_slice()),
            ("selected-dir/child".into(), b"selected directory content\0".as_slice()),
            ("selected-dir/deeper/child".into(), b"selected nested content\xff".as_slice()),
            (".ssh/secret".into(), b"synthetic credential: must never cross the view".as_slice()),
            ("write-only".into(), b"previous write-only\0\xff".as_slice()),
            ("atomic".into(), b"atomic-before\0\xfe".as_slice()),
            ("read-only".into(), b"selected read-only content\0".as_slice()),
            ("watched".into(), b"watch-before\0".as_slice()),
            ("exec-target".into(), b"#!/bin/sh\nexit 0\n".as_slice()),
        ] {
            let path = source.join(name);
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(&path, Permissions::from_mode(0o600)).unwrap();
            own(&path);
        }
        std::fs::set_permissions(source.join("exec-target"), Permissions::from_mode(0o700)).unwrap();
        std::fs::hard_link(source.join(".ssh/secret"), source.join("hardlink-alias")).unwrap();
        std::os::unix::fs::symlink(".ssh/secret", source.join("symlink-alias")).unwrap();
        std::fs::write(source.join("owner-denied"), b"synthetic Root-only file").unwrap();
        std::fs::set_permissions(source.join("owner-denied"), Permissions::from_mode(0o600)).unwrap();

        let duration = Duration::from_secs(if ending == "expiry" { 15 } else if ending == "write" { 120 } else { 60 });
        let context = crate::paths::RoutedPathContext::for_owner(OWNER, PathBuf::from("/run/display-home"));
        let sid = if ending == "write" {
            Some(context.clone().scope_sync(|| crate::session::create("private selected native writes")).unwrap())
        } else { None };
        let mut view = match &sid {
            Some(sid) => NativeFsView::create_writable(OWNER, &source, &mountpoint, duration, sid).unwrap(),
            None => NativeFsView::create(OWNER, &source, &mountpoint, duration).unwrap(),
        };
        view.health().unwrap();
        for denied in [Path::new(".ssh/secret"), Path::new("symlink-alias"), Path::new("hardlink-alias"),
            Path::new("../outside"), Path::new("owner-denied")]
        {
            assert!(view.choose_read(denied).is_err(), "Root selection unexpectedly admitted {denied:?}");
        }
        let group = session_group(cgroup, &format!("native-fs-{ending}"));
        let group_path = cgroup.join(format!("login-native-fs-{ending}"));
        std::fs::write(group_path.join("pids.max"), b"16").unwrap();
        std::fs::write(group_path.join("memory.max"), b"268435456").unwrap();
        std::fs::write(group_path.join("cpu.max"), b"100000 100000").unwrap();
        let directory = claw_display_control::workload::open_directory(&group_path).unwrap();
        let entry_token = uuid::Uuid::new_v4().simple().to_string();
        let policy = LaunchPolicy {
            tier: TrustTier::AppOperation, label: "private-native-fs-kernel".into(),
            program: "/usr/local/bin/claw-app-runner".into(),
            argv: vec!["--launch-gate".into(), entry_token.clone(), "--".into(),
                "/usr/lib/cos/bin/claw-gui-probe".into(), "native-fs-kernel".into(), "/resource-view".into()],
            workdir: "/data".into(),
            mounts: vec![if ending == "write" {
                    Mount::read_write(&mountpoint, "/resource-view", MountClass::Output)
                } else { Mount::read_only(&mountpoint, "/resource-view", MountClass::Input) },
                Mount::read_write(&data, "/data", MountClass::AppData)],
            network: NetworkPolicy::Denied,
            env: [
                ("PATH", "/usr/bin:/bin"), ("HOME", "/data"), ("XDG_CONFIG_HOME", "/data/config"),
                ("XDG_DATA_HOME", "/data/share"), ("XDG_CACHE_HOME", "/data/cache"),
                ("COS_OWNER_HOME", "/run/display-home"),
            ].into_iter().map(|(key, value)| (key.into(), value.into())).collect(),
            limits: crate::worker::policy::Limits { memory_bytes: 268435456, pids_max: 16,
                runtime: duration, ..crate::worker::policy::Limits::operation() },
            seccomp: SeccompProfile::Strict, stdio: StdioPlan::Streamed, broker: false, umask: 0o077,
        };
        let (mut prepared, gate) = crate::worker::linux::prepare_native_fs_fixture(&policy, OWNER, &group).unwrap();
        let mut child = prepared.command.stdin(Stdio::piped()).stdout(Stdio::piped())
            .stderr(Stdio::inherit()).spawn().unwrap();
        view.bind(&ProcessIdentity::child(&child).unwrap(), directory).unwrap();
        gate.release().unwrap();
        child.stdin.as_mut().unwrap().write_all(entry_token.as_bytes()).unwrap();
        receive(&mut child, b"META");
        // This fixture is the trusted selection controller; probe tokens only
        // synchronize fixed stages and never choose resources or grant rights.
        view.choose_read(Path::new("binary")).unwrap();
        view.choose_read(Path::new(&raw)).unwrap();
        child.stdin.as_mut().unwrap().write_all(b"F").unwrap();
        receive(&mut child, b"FILE");
        view.choose_read(Path::new("selected-dir")).unwrap();
        child.stdin.as_mut().unwrap().write_all(b"D").unwrap();
        receive(&mut child, b"DIRS");
        if ending == "write" {
            for denied in [Path::new(".ssh/secret"), Path::new("symlink-alias"), Path::new("hardlink-alias"),
                Path::new("../outside"), Path::new("owner-denied"), Path::new("selected-dir"), Path::new(&raw)]
            {
                assert!(view.choose_write(denied).is_err(), "write selection unexpectedly admitted {denied:?}");
            }
            for target in ["write-only", "atomic", "exec-target"] { view.choose_write(Path::new(target)).unwrap(); }
            for target in ["read-only", "atomic", "watched"] { view.choose_read(Path::new(target)).unwrap(); }
            view.choose_watch(Path::new("watched")).unwrap();
            let saves = [view.reserve_save(Path::new("atomic")).unwrap(), view.reserve_save(Path::new("atomic")).unwrap()];
            child.stdin.as_mut().unwrap().write_all(b"W").unwrap();
            for save in &saves {
                let name = save.as_os_str().as_bytes();
                child.stdin.as_mut().unwrap().write_all(&(u16::try_from(name.len()).unwrap()).to_le_bytes()).unwrap();
                child.stdin.as_mut().unwrap().write_all(name).unwrap();
            }
            receive(&mut child, b"WROK");
            let mut written: Vec<u8> = (0_usize..2 * 1024 * 1024 + 29)
                .map(|index| index.wrapping_mul(13).wrapping_add(29) as u8).collect();
            written[12345..12352].copy_from_slice(b"\0\xffpatch");
            assert_eq!(std::fs::read(source.join("write-only")).unwrap(), written);
            let mut saved: Vec<u8> = (0_usize..262167).map(|index| index.wrapping_mul(19) as u8).collect();
            saved[777..779].copy_from_slice(b"\0\xfd");
            assert_eq!(std::fs::read(source.join("atomic")).unwrap(), saved);
            assert_eq!(std::fs::read(source.join("unselected")).unwrap(), b"unselected content");
            assert!(!source.join("arbitrary-sibling").exists());
            for save in saves { assert!(!source.join(save).exists(), "view-only scratch escaped into owner siblings"); }
            let sid = sid.as_ref().unwrap();
            let mutations = context.clone().scope_sync(|| crate::session::iter_mutations(sid)).unwrap();
            let atomic = mutations.iter().find_map(|record| match &record.mutation {
                crate::session::Mutation::FsWrite { path, prev_blob: Some(blob) }
                    if Path::new(path) == source.join("atomic") => Some(blob),
                _ => None,
            }).expect("canonical inverse for atomic save");
            assert_eq!(context.clone().scope_sync(|| crate::session::read_blob(sid, atomic)).unwrap(), b"atomic-before\0\xfe");
            let partition = crate::session::journal::Partition::Session(sid.clone());
            let projection = context.clone().scope_sync(|| crate::session::journal::projection::build(&partition, OWNER)).unwrap();
            assert!(projection.health.is_verified());
            assert!(projection.mutations.len() >= 4);
            assert!(projection.mutations.iter().all(|effect| effect.status == "committed"
                && effect.session_mutation.is_some_and(|seq| mutations.iter().any(|record| record.seq == seq))));
            assert!(projection.mutations.iter().any(|effect|
                effect.route == crate::clawd::routes::Command::SystemFileReplace.route().name));

            receive(&mut child, b"WATC");
            {
                let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
                std::fs::write(source.join("watched"), b"watch-after\0\xffexternal").unwrap();
                std::fs::write(source.join("external-entry"), b"metadata only: no read grant").unwrap();
            }
            child.stdin.as_mut().unwrap().write_all(b"C").unwrap();
            receive(&mut child, b"CHNG");
            {
                let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
                std::fs::remove_file(source.join("watched")).unwrap();
            }
            receive(&mut child, b"GONE");
        } else {
            assert!(view.choose_write(Path::new("binary")).is_err());
            child.stdin.as_mut().unwrap().write_all(b"H").unwrap();
        }
        receive(&mut child, b"HELD");
        let held: Vec<OwnedFd> = std::fs::read_to_string(group_path.join("cgroup.procs")).unwrap()
            .lines().map(|value| {
                let pid: i32 = value.parse().unwrap();
                let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
                assert!(fd >= 0);
                unsafe { OwnedFd::from_raw_fd(fd as i32) }
            }).collect();
        assert!(held.len() >= 3, "probe must retain an escaped process-tree descendant");
        match ending {
            "revoke" => view.revoke().unwrap(),
            "worker-failure" => view.fail_worker_for_fixture().unwrap(),
            "write" => {
                let before = std::fs::read(source.join("write-only")).unwrap();
                crate::session::journal::faults::arm(crate::session::journal::faults::Fault::AppendWrite);
                child.stdin.as_mut().unwrap().write_all(b"F").unwrap();
                receive(&mut child, b"DENY");
                crate::session::journal::faults::disarm();
                assert_eq!(std::fs::read(source.join("write-only")).unwrap(), before);
                view.fail_completion_for_fixture().unwrap();
                child.stdin.as_mut().unwrap().write_all(b"U").unwrap();
                receive(&mut child, b"UNKN");
            }
            "expiry" => {}
            _ => unreachable!(),
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() { break status; }
            assert!(Instant::now() < deadline, "native view did not retire its retained-FD process tree");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(!status.success());
        assert!(view.health().is_err());
        if let Some(sid) = &sid {
            let partition = crate::session::journal::Partition::Session(sid.clone());
            let unresolved = context.clone().scope_sync(|| crate::session::journal::unresolved_mutations(&partition, OWNER)).unwrap();
            assert_eq!(unresolved.len(), 1, "failed completion must leave canonical replay refusal");
            assert_eq!(std::fs::read(source.join("write-only")).unwrap()[3], 0xab);
        }
        claw_display_control::workload::retire_cgroup(
            claw_display_control::workload::open_directory(&group_path).unwrap().as_fd(), deadline,
        ).unwrap();
        for fd in held {
            let mut poll = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1, "retained process identity survived retirement");
        }
        // Provider source pins are released only after checked process death.
        drop(prepared.resources);
        view.retire_checked(deadline).unwrap();
        assert!(std::fs::read_dir(&mountpoint).unwrap().next().is_none());
        assert!(!source.join("must-not-exist").exists());
        assert_eq!(std::fs::read(source.join("binary")).unwrap(), binary);
        assert!(!std::fs::read_dir(&source).unwrap().any(|entry|
            entry.unwrap().file_name().as_bytes().starts_with(b".cos-native-")));
        println!("private native FUSE metadata/selection/mmap/{ending} custody passed");
    }
}

fn private_gui_fixture(session_only: bool, file_manager_only: bool) {
    private_gui_fixture_selected(session_only, file_manager_only, false, None);
}

fn private_gui_fixture_selected(
    session_only: bool,
    file_manager_only: bool,
    package_only: bool,
    native: Option<native_products::Product>,
) {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    if package_only {
        package::require_namespaces();
    } else {
        assert_ne!(
            std::fs::read_link("/proc/self/ns/mnt").unwrap(),
            std::fs::read_link("/proc/1/ns/mnt").unwrap()
        );
    }
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
    install_packages(package_only, native);
    let _package_repository = package_only.then(|| package::Repository::start(native.is_some()));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let state = DaemonState::try_new().unwrap();
        let admission = Admission::new(Limits::default());
        let _package_custody = package_only.then(|| package::custody(&state, &admission));
        let _manager = crate::clawd::gui::Manager::start(state.clone(), admission.clone()).unwrap();
        let _registry = crate::display_session::registry::Registry::start().unwrap();
        let socket = Path::new("/run/cos/clawd.sock");
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        enable_credential_passing(&listener, socket).unwrap();
        if package_only {
            std::os::unix::fs::chown(socket, None, Some(OWNER)).unwrap();
            std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o660)).unwrap();
        } else {
            std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o666)).unwrap();
        }
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
        if package_only && native.is_none() {
            package::run(&session_group(root, "22")).await;
        }
        if let Some(product) = native {
            native_products::run(&session_group(root, &product.case.to_string()), product).await;
        }
        if !package_only && native.is_none() && !session_only && !file_manager_only {
            headless_cli(&session_group(root, "headless"));
            activation_cases(root);
            for case in (0..20).chain([23, 20, 21, 24]) {
                let group = session_group(root, &case.to_string());
                run_case(case, &group).await;
            }
        }
        if !package_only && native.is_none() && !session_only {
            file_manager::run(root).await;
        }
        if !package_only && native.is_none() && !file_manager_only {
            session_startup_failure(root);
        }
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    });
}
