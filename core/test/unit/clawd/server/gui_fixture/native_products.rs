use super::*;
use std::os::unix::fs::MetadataExt;

const APP: &str = "cosmic-edit";
const ENTRY: &str = "/usr/lib/cos/apps/cosmic-edit/bin/cosmic-edit";
const OBSERVER: &str = "/run/user/62050/native-editor-observer/window.json";
const TARGET: &str = "/run/display-home/native-editor-startup.txt";
const CONTENT: &[u8] =
    b"Actual product-owned Editor startup.\nMetadata consent is not content consent.\n";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    schema: String,
    source_lock: serde_json::Value,
    platform_lock: serde_json::Value,
    entrypoints: Vec<String>,
    resources: Vec<String>,
    payload_inventory: Vec<crate::provenance::envelope::FileEntry>,
}

pub(super) fn install(key: &crate::provenance::sign::SigningKeyFile) {
    use crate::provenance::{
        envelope::PackageKind,
        sign::{sign_directory, SignRequest},
        verify::{verify_package, VerifyOptions},
    };

    let receipt = Path::new("/run/gui-fixture/native-editor.json");
    let metadata = std::fs::symlink_metadata(receipt).unwrap();
    assert!(metadata.is_file() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0);
    assert!(metadata.len() <= 2 * 1024 * 1024);
    let mut payload: Payload = serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap();
    assert_eq!(payload.schema, "claw.private-native-editor/v1");
    assert_eq!(payload.source_lock["version"], 1);
    let revision = payload.source_lock["revision"].as_str().unwrap();
    assert!(
        revision.len() == 40
            && revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    let selection: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../packaging/apps.lock.json"
    )))
    .unwrap();
    assert_eq!(payload.source_lock["repository"], selection["repository"]);
    assert_eq!(payload.platform_lock["version"], "0.2.1");
    assert_eq!(payload.platform_lock["runtime_abi"], 1);
    assert_eq!(
        payload.platform_lock["url"],
        "https://github.com/xiaoyu-work/claw-os/releases/download/app-platform-v0.2.1/claw-os-app-platform-0.2.1.tar.gz"
    );
    let platform_digest = payload.platform_lock["sha256"].as_str().unwrap();
    assert!(
        platform_digest.len() == 64
            && platform_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    assert_eq!(payload.entrypoints, ["bin/cosmic-edit"]);
    for required in [
        "resources/usr/share/applications/com.clawos.Edit.desktop",
        "resources/usr/share/metainfo/com.clawos.Edit.metainfo.xml",
        "resources/usr/share/icons/hicolor/scalable/apps/com.clawos.Edit.svg",
    ] {
        assert!(payload
            .resources
            .iter()
            .any(|resource| resource == required));
    }
    let dir = Path::new("/usr/lib/cos/apps/cosmic-edit");
    assert!(
        !dir.join(".provenance.json").exists(),
        "fixture input was already signed"
    );
    assert_eq!(std::fs::read(ENTRY).unwrap()[..7], *b"\x7fELF\x02\x01\x01");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("app.json")).unwrap()).unwrap();
    assert_eq!(manifest["id"], APP);
    assert_eq!(manifest["schema_version"], 2);
    sign_directory(
        dir,
        &SignRequest {
            kind: PackageKind::App,
            id: APP.into(),
            version: manifest["version"].as_str().unwrap().into(),
            manifest_schema: "cos.app-manifest/v2".into(),
            manifest_path: "app.json".into(),
            entrypoints: payload.entrypoints,
            resources: payload.resources,
        },
        key,
    )
    .unwrap();
    std::fs::set_permissions(
        dir.join(".provenance.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let package = verify_package(
        dir,
        &VerifyOptions::new(PackageKind::App).expect_id(APP),
        &crate::provenance::trust_store(),
    )
    .unwrap();
    let mut signed = package.files().cloned().collect::<Vec<_>>();
    signed.sort_by(|left, right| left.path.cmp(&right.path));
    payload
        .payload_inventory
        .sort_by(|left, right| left.path.cmp(&right.path));
    assert_eq!(
        signed, payload.payload_inventory,
        "signed App must retain every actual installer/resource/license byte and mode"
    );
    let review = crate::approvals::system_review::submit(
        OWNER,
        crate::approvals::system_review::ReviewKind::AppActivation,
        &package,
        "private real Editor acceptance controller".into(),
    )
    .unwrap();
    crate::approvals::system_review::decide(OWNER, &review.id, true, Some(&package)).unwrap();
    println!(
        "private actual Editor input: App revision {revision}, public platform {platform_digest}, signed ELF/resources/licenses verified"
    );
}

fn diagnostic(stage: &str) -> String {
    let log = std::fs::read("/run/gui-fixture/pam-native-editor.log").unwrap_or_default();
    let tail = &log[log.len().saturating_sub(8192)..];
    format!(
        "actual Editor gate {stage}: {}{}",
        String::from_utf8_lossy(tail),
        private_audit_tail()
    )
}

fn observe() -> Option<serde_json::Value> {
    match std::fs::symlink_metadata(OBSERVER) {
        Ok(metadata) => {
            assert!(
                metadata.is_file()
                    && metadata.uid() == OWNER
                    && metadata.mode() & 0o077 == 0
                    && metadata.len() <= 8192
            );
            Some(serde_json::from_slice(&std::fs::read(OBSERVER).unwrap()).unwrap())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("read protected compositor observation: {error}"),
    }
}

fn buffered_window(record: &serde_json::Value, instance: &str) -> bool {
    record["schema"] == "claw.private-native-window/v1"
        && record["instance"] == instance
        && record["toplevels"] == 1
        && record["configure_acked"] == true
        && record["closed"] == false
        && record["buffer_commits"]
            .as_u64()
            .is_some_and(|count| count > 0)
        && record["raster"]["width"]
            .as_u64()
            .is_some_and(|size| size >= 100)
        && record["raster"]["height"]
            .as_u64()
            .is_some_and(|size| size >= 100)
        && record["raster"]["nonuniform_pixels"] == true
}

fn loaded(record: &serde_json::Value) -> bool {
    record["title"]
        .as_str()
        .is_some_and(|title| title.starts_with("native-editor-startup.txt - "))
}

struct EditorProcess {
    identity: ProcessIdentity,
    pidfd: OwnedFd,
    inode: (u64, u64),
}

fn host_live(retained: &RetainedInstance, stage: &str) {
    assert!(
        !claw_display_control::identity::pidfd_exited(retained.process.as_fd()).unwrap(),
        "{}",
        diagnostic(stage)
    );
    let process = ProcessIdentity::from_pidfd(retained.process.as_fd()).unwrap();
    assert_eq!(process.uid(), OWNER);
}

fn find_editor(retained: &RetainedInstance) -> Option<EditorProcess> {
    let group =
        std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd())).unwrap();
    let expected = std::fs::metadata(ENTRY).unwrap();
    let mut matched = Vec::new();
    // The registered Host pid can be bubblewrap, not the native executable.
    for pid in std::fs::read_to_string(group.join("cgroup.procs"))
        .unwrap()
        .lines()
    {
        let pid: i32 = pid.parse().unwrap();
        let Ok(running) = std::fs::metadata(format!("/proc/{pid}/exe")) else {
            continue;
        };
        if running.dev() != expected.dev() || running.ino() != expected.ino() {
            continue;
        }
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
        assert!(
            raw >= 0,
            "actual Editor exited while its ELF identity was being retained"
        );
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        let identity = ProcessIdentity::from_pidfd(pidfd.as_fd()).unwrap();
        assert_eq!(identity.uid(), OWNER);
        identity.assert_current().unwrap();
        let current = std::fs::metadata(format!("/proc/{pid}/exe")).unwrap();
        assert_eq!(
            (current.dev(), current.ino()),
            (expected.dev(), expected.ino())
        );
        matched.push(EditorProcess {
            identity,
            pidfd,
            inode: (current.dev(), current.ino()),
        });
    }
    assert!(
        matched.len() <= 1,
        "actual Editor daemonized into multiple native processes"
    );
    matched.pop()
}

fn native_process(retained: &RetainedInstance, editor: &EditorProcess, stage: &str) {
    host_live(retained, stage);
    assert!(
        !claw_display_control::identity::pidfd_exited(editor.pidfd.as_fd()).unwrap(),
        "{}",
        diagnostic(stage)
    );
    editor.identity.assert_current().unwrap();
    let running = std::fs::metadata(format!("/proc/{}/exe", editor.identity.pid())).unwrap();
    assert_eq!((running.dev(), running.ino()), editor.inode);
}

fn launcher_output_tail() -> std::io::Result<serde_json::Value> {
    use std::io::{Read, Seek, SeekFrom};

    const LIMIT: u64 = 16384;
    let mut file = std::fs::File::open("/run/gui-fixture/pam-native-editor.log")?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(LIMIT)))?;
    let mut bytes = Vec::new();
    file.take(LIMIT).read_to_end(&mut bytes)?;
    Ok(serde_json::json!({
        "untrusted_ordinary_launcher_output": String::from_utf8_lossy(&bytes),
        "log_bytes": length, "tail_truncated": length > LIMIT,
    }))
}

async fn renderer_failure(
    control: &OwnedFd,
    pam: &mut Child,
    retained: &RetainedInstance,
    editor: &EditorProcess,
    session: &str,
    stage: &str,
) -> ! {
    let original = diagnostic(stage);
    let window = claw_display_control::Epoch(retained.instance.0).label();
    let pending = crate::approvals::list_pending_for_owner(Some(OWNER))
        .into_iter()
        .filter(|request| {
            request.verb == "fs.read" && request.session.starts_with(&format!("gui-read-{window}-"))
        })
        .count();
    eprintln!(
        "private actual Editor renderer failure before cleanup {}",
        serde_json::json!({
            "stage": stage, "readiness_gate_failed": true, "window": window,
            "pending_read_reviews": pending, "witness": observe(),
        })
    );

    // Keep the broker alive until the ordinary owner CLI can receive its checked outcome.
    let stopped = tokio::time::timeout(
        Duration::from_secs(5),
        crate::clawd::client::request(
            "/run/cos/clawd.sock",
            crate::clawd::wire::Request::new(
                crate::clawd::routes::Command::PermissionRevoke,
                serde_json::json!({"owner_uid": OWNER, "session": session}),
            ),
        ),
    )
    .await;
    let (revoked, error) = match stopped {
        Ok(Ok(reply)) if reply.ok => (true, None),
        Ok(Ok(reply)) => (false, Some(format!("{:?}", reply.error))),
        Ok(Err(error)) => (false, Some(error.to_string())),
        Err(_) => (
            false,
            Some("checked failure retirement deadline".to_string()),
        ),
    };
    let host_exited =
        claw_display_control::identity::pidfd_exited(retained.process.as_fd()).unwrap();
    let editor_exited = claw_display_control::identity::pidfd_exited(editor.pidfd.as_fd()).unwrap();
    let group_deleted = std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd()))
        .unwrap()
        .as_os_str()
        .as_encoded_bytes()
        .ends_with(b" (deleted)");
    let runtime = Path::new("/run/cos/gui").join(&window);
    let mountpoint = runtime.join("home-metadata");
    let runtime_removed = !runtime.exists()
        && !mountpoint.exists()
        && !std::fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .lines()
            .any(|line| line.split_whitespace().nth(4) == mountpoint.to_str());
    let retired = revoked && host_exited && editor_exited && group_deleted && runtime_removed;
    let mut report = serde_json::json!({
        "stage": stage, "checked_retired": retired, "retirement_error": error,
        "host_exited": host_exited, "editor_exited": editor_exited,
        "group_deleted": group_deleted, "runtime_removed": runtime_removed,
    });
    if retired {
        command(control, b"WAIT");
        let ended = status(control, pam, Instant::now() + Duration::from_secs(5));
        report["launcher_status"] = String::from_utf8_lossy(&ended).into_owned().into();
        report["ordinary_output_after_checked_retirement"] = launcher_output_tail()
            .unwrap_or_else(|error| serde_json::json!({"unavailable": error.to_string()}));
        command(control, b"CLSE");
        let deadline = Instant::now() + Duration::from_secs(5);
        let pam_end = loop {
            if let Some(ended) = pam.try_wait().unwrap() {
                break serde_json::json!({"exited": true, "exit_code": ended.code()});
            }
            if Instant::now() >= deadline {
                break serde_json::json!({"exited": false});
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        report["pam"] = pam_end;
        report["retired_witness"] = observe().unwrap_or(serde_json::Value::Null);
    }
    // JSON escaping keeps captured App text inert in the CI log.
    eprintln!("private actual Editor renderer failure after cleanup {report}");
    panic!("{original}");
}

fn startup_authority(session: &str, parent: &str) -> (CapSet, CapSet) {
    crate::paths::RoutedPathContext::for_owner(OWNER, Path::new("/run/display-home").to_path_buf())
        .scope_sync(|| {
            let gui = crate::proc::session_info_by_id(session).unwrap();
            assert_eq!(gui.app_id.as_deref(), Some(APP));
            let gui = gui.caps.unwrap();
            let parent = crate::proc::session_info_by_id(parent)
                .unwrap()
                .caps
                .unwrap();
            let mut metadata = CapSet::new();
            metadata.insert(Cap::new(Verb::FS_META, Scope::path("/run/display-home/**")));
            assert_eq!(
                gui, metadata,
                "optional read/write/AI must not become startup authority"
            );
            let expected_parent = CapSet::from_caps([
                Cap::new(Verb::AGENT_INVOKE, Scope::name(APP)),
                Cap::new(Verb::FS_META, Scope::path("/run/display-home/**")),
            ]);
            assert_eq!(
                parent, expected_parent,
                "launcher must not borrow a model, Admin or content grant"
            );
            (gui, parent)
        })
}

pub(super) async fn run(group: &std::fs::File) {
    let home = Path::new("/run/display-home");
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
        std::fs::write(TARGET, CONTENT).unwrap();
        std::fs::write(
            home.join("native-editor-unselected.txt"),
            b"unselected content stays private\n",
        )
        .unwrap();
    }
    assert!(
        observe().is_none(),
        "stale window observations cannot satisfy a new launch"
    );
    let (control, child_control) = bootstrap::pair().unwrap();
    let mut pam = spawn_pam("gui", "native-editor", group, Stdio::from(child_control));
    assert_eq!(
        status(&control, &pam, Instant::now() + Duration::from_secs(30)),
        *b"AUTH"
    );
    command(&control, b"CASE");
    command(&control, b"25..");
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
        if process.uid() == OWNER && process.gid() == OWNER {
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
    let parent = launch_parent(&launcher, 25);
    command(&control, b"RUN!");
    let deadline = Instant::now() + Duration::from_secs(20);
    let metadata = loop {
        if let Some(request) = crate::approvals::list_pending_for_owner(Some(OWNER))
            .into_iter()
            .find(|request| {
                request.verb == "fs.meta"
                    && request.scope == Scope::path("/run/display-home/**")
                    && request.session.starts_with("app-")
                    && request.operation_digest.is_some()
            })
        {
            break request;
        }
        assert!(
            pam.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "{}",
            diagnostic("per-window metadata review")
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_ne!(metadata.session, parent);
    assert!(
        observe().is_none(),
        "actual App code ran before per-window metadata consent"
    );
    assert!(
        !crate::approvals::list_pending_for_owner(Some(OWNER))
            .iter()
            .any(|request| request.verb == "fs.read"),
        "content selection preceded metadata consent"
    );
    approve_native_resource(&metadata.id).await;

    let deadline = Instant::now() + Duration::from_secs(25);
    let retained = loop {
        let bound = crate::paths::RoutedPathContext::for_owner(OWNER, home.to_path_buf())
            .scope_sync(|| {
                crate::proc::session_info_by_id(&metadata.session)
                    .is_some_and(|session| !session.pending_bind && session.pid > 1)
            });
        if bound
            && Path::new("/run/cos/gui")
                .read_dir()
                .unwrap()
                .next()
                .is_some()
        {
            break retained_instance(&metadata.session);
        }
        assert!(
            pam.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "{}",
            diagnostic("common Host process binding")
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let window = claw_display_control::Epoch(retained.instance.0).label();
    let runtime = Path::new("/run/cos/gui").join(&window);
    let mountpoint = runtime.join("home-metadata");
    let process = loop {
        host_live(&retained, "signed native ELF startup/foreground lifetime");
        if let Some(process) = find_editor(&retained) {
            break process;
        }
        assert!(
            Instant::now() < deadline,
            "{}",
            diagnostic("exact signed Editor ELF execution")
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let original = startup_authority(&metadata.session, &parent);
    let environment = std::fs::read(format!("/proc/{}/environ", process.identity.pid())).unwrap();
    let value = |key: &str| {
        environment
            .split(|byte| *byte == 0)
            .find_map(|entry| entry.strip_prefix(format!("{key}=").as_bytes()))
    };
    assert_eq!(value("COS_APP_GUI"), Some(b"1".as_slice()));
    assert_eq!(value("ICED_BACKEND"), Some(b"tiny-skia".as_slice()));
    assert_eq!(value("COS_COMMAND"), Some(b"--gui".as_slice()));
    assert_eq!(value("COS_SESSION"), Some(metadata.session.as_bytes()));
    assert_eq!(
        value("COS_OWNER_HOME"),
        Some(b"/run/display-home".as_slice())
    );
    assert_eq!(
        value("COS_APP_RESOURCE_DIR"),
        Some(b"/usr/lib/cos/apps/cosmic-edit/resources".as_slice())
    );
    assert!(value("XDG_DATA_DIRS")
        .unwrap()
        .starts_with(b"/usr/lib/cos/apps/cosmic-edit/resources/usr/share:"));
    assert_ne!(value("HOME"), Some(b"/run/display-home".as_slice()));
    assert_ne!(value("COS_MCP_SERVER"), Some(b"1".as_slice()));
    let ungranted: Vec<usize> =
        serde_json::from_slice(value("COS_APP_OPTIONAL_NEEDS_UNGRANTED_JSON").unwrap()).unwrap();
    assert_eq!(ungranted, [0, 1, 2]);
    assert_eq!(
        value("COS_ARGS_JSON"),
        Some(format!("[\"{TARGET}\"]").as_bytes())
    );

    let mut read = None;
    let before = loop {
        native_process(
            &retained,
            &process,
            "actual configured/buffered window and startup read request",
        );
        if read.is_none() {
            read = crate::approvals::list_pending_for_owner(Some(OWNER))
                .into_iter()
                .find(|request| {
                    request.verb == "fs.read"
                        && request.session.starts_with(&format!("gui-read-{window}-"))
                });
        }
        if let Some(record) =
            observe().filter(|record| buffered_window(record, &window) && read.is_some())
        {
            break record;
        }
        if pam.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            renderer_failure(
                &control,
                &mut pam,
                &retained,
                &process,
                &metadata.session,
                "actual xdg_toplevel configure/SHM frame and independent fs.read",
            )
            .await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        !loaded(&before),
        "startup file was loaded before independent content consent"
    );
    assert!(matches!(
        crate::approvals::status_for_owner(&metadata.id, Some(OWNER)),
        crate::approvals::RequestStatus::Consumed
    ));
    let read = read.unwrap();
    assert_eq!(read.owner_uid, Some(OWNER));
    assert_eq!(read.scope, Scope::path("/run/display-home/**"));
    assert_ne!(read.session, metadata.session);
    assert!(read.operation_digest.is_some() && read.operation_digest != metadata.operation_digest);
    println!("private actual Editor window ready; separate startup-file fs.read remains pending");
    approve_native_resource(&read.id).await;
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        native_process(
            &retained,
            &process,
            "actual native file load after independent consent",
        );
        if observe().is_some_and(|record| {
            buffered_window(&record, &window)
                && loaded(&record)
                && record["buffer_commits"].as_u64().unwrap()
                    > before["buffer_commits"].as_u64().unwrap()
        }) {
            break;
        }
        if Instant::now() >= deadline {
            renderer_failure(
                &control,
                &mut pam,
                &retained,
                &process,
                &metadata.session,
                "actual loaded-tab title and a newly rendered native frame",
            )
            .await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(matches!(
        crate::approvals::status_for_owner(&read.id, Some(OWNER)),
        crate::approvals::RequestStatus::Consumed
    ));
    assert_eq!(startup_authority(&metadata.session, &parent), original);
    assert!(
        std::fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .lines()
            .any(|line| line.split_whitespace().nth(4) == mountpoint.to_str()),
        "actual Editor must use the controlled native metadata/content mount"
    );
    let group_path =
        std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd())).unwrap();
    let processes = std::fs::read_to_string(group_path.join("cgroup.procs"))
        .unwrap()
        .lines()
        .map(|pid| {
            let raw =
                unsafe { libc::syscall(libc::SYS_pidfd_open, pid.parse::<i32>().unwrap(), 0_u32) };
            assert!(raw >= 0);
            unsafe { OwnedFd::from_raw_fd(raw as i32) }
        })
        .collect::<Vec<_>>();
    assert!(!processes.is_empty());
    let generation =
        crate::approvals::generations::current(Some(OWNER), &metadata.session).unwrap();
    let revoked = crate::clawd::client::request(
        "/run/cos/clawd.sock",
        crate::clawd::wire::Request::new(
            crate::clawd::routes::Command::PermissionRevoke,
            serde_json::json!({"owner_uid": OWNER, "session": read.session}),
        ),
    )
    .await
    .unwrap();
    assert!(
        revoked.ok,
        "actual Editor selected-read retirement failed: {:?}",
        revoked.error
    );
    assert_eq!(
        crate::approvals::generations::current(Some(OWNER), &metadata.session).unwrap(),
        generation
    );
    assert!(claw_display_control::identity::pidfd_exited(retained.process.as_fd()).unwrap());
    assert!(claw_display_control::identity::pidfd_exited(process.pidfd.as_fd()).unwrap());
    for process in processes {
        assert!(claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap());
    }
    assert!(
        std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd()))
            .unwrap()
            .as_os_str()
            .as_encoded_bytes()
            .ends_with(b" (deleted)")
    );
    assert!(
        !runtime.exists() && !mountpoint.exists(),
        "actual Editor runtime/provider cleanup was not acknowledged"
    );
    assert!(!std::fs::read_to_string("/proc/self/mountinfo")
        .unwrap()
        .lines()
        .any(|line| line.split_whitespace().nth(4) == mountpoint.to_str()));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if observe().is_some_and(|record| {
            record["instance"] == window && record["closed"] == true && record["toplevels"] == 0
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual Editor Wayland window did not close"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(std::fs::read(TARGET).unwrap(), CONTENT);
    assert_eq!(
        std::fs::read(home.join("native-editor-unselected.txt")).unwrap(),
        b"unselected content stays private\n"
    );
    command(&control, b"WAIT");
    assert_eq!(
        status(&control, &pam, Instant::now() + Duration::from_secs(30)),
        *b"FAIL"
    );
    command(&control, b"CLSE");
    assert!(pam.wait().unwrap().success());
    let login_group = std::fs::read_link(format!("/proc/self/fd/{}", group.as_raw_fd())).unwrap();
    assert!(
        std::fs::read_to_string(login_group.parent().unwrap().join("cgroup.events"))
            .unwrap()
            .lines()
            .any(|line| line == "populated 0")
    );
    println!(
        "private actual Editor: real signed ELF/resources, native window/file load and checked process/runtime/PAM retirement passed; Save As not exercised"
    );
}

#[test]
fn native_editor_readiness_rejects_titles_without_real_configured_pixels() {
    let mut record = serde_json::json!({
        "schema": "claw.private-native-window/v1", "instance": "window",
        "toplevels": 1, "configure_acked": true, "closed": false,
        "buffer_commits": 1, "title": "native-editor-startup.txt - Text Editor",
    });
    assert!(!buffered_window(&record, "window"));
    record["raster"] = serde_json::json!({"width": 800, "height": 600, "nonuniform_pixels": false});
    assert!(!buffered_window(&record, "window"));
    record["raster"]["nonuniform_pixels"] = true.into();
    record["render_trace"] = serde_json::json!({
        "top_pre_commits": 3, "top_pending_buffers": 3,
        "top_frame_callbacks_sent": 3, "configure_acks": 1,
    });
    record["buffer_commits"] = 0.into();
    assert!(
        !buffered_window(&record, "window"),
        "commit/callback diagnostics cannot substitute for an actual observed pixel buffer"
    );
    record["buffer_commits"] = 1.into();
    assert!(buffered_window(&record, "window") && loaded(&record));
    assert!(!buffered_window(&record, "another-window"));
    record["configure_acked"] = false.into();
    assert!(!buffered_window(&record, "window"));
}

#[test]
#[ignore = "clean-GHA-only: actual Editor ELF, authenticated private PAM/display, FUSE and cgroup v2"]
fn private_native_editor() {
    private_gui_fixture_selected(false, false, false, true);
}
