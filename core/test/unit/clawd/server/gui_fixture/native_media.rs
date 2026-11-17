use super::*;
use std::collections::HashSet;
use std::os::unix::fs::FileTypeExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const INPUT: &str = "/run/display-home/native-player.webm";
const CONFIG: &str = r#"
context.properties = { core.daemon = true core.name = pipewire-0 default.clock.rate = 48000 }
context.spa-libs = {
    audio.convert.* = audioconvert/libspa-audioconvert
    support.* = support/libspa-support
}
context.modules = [
    { name = libpipewire-module-protocol-native }
    { name = libpipewire-module-metadata }
    { name = libpipewire-module-spa-node-factory }
    { name = libpipewire-module-client-node }
    { name = libpipewire-module-access }
    { name = libpipewire-module-adapter }
    { name = libpipewire-module-link-factory }
]
context.objects = [
    { factory = adapter args = {
        factory.name = support.null-audio-sink
        node.name = claw-private-null
        media.class = Audio/Sink
        audio.position = [ FL FR ]
        object.linger = true
        adapter.auto-port-config = { mode = dsp monitor = false position = preserve }
    } }
]
"#;

struct OwnedChild(std::process::Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|status| status.is_some()) {
            return;
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn owner_command(program: &str) -> std::process::Command {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(program);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", "/run/display-home")
        .env("XDG_RUNTIME_DIR", "/run/user/62050")
        .env("PIPEWIRE_RUNTIME_DIR", "/run/user/62050")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
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
    command
}

fn owner_tool(program: &str) -> std::process::Command {
    let mut command = owner_command("/usr/bin/timeout");
    command.args(["--signal=KILL", "3", program]);
    command
}

pub(super) struct Fixture {
    server: OwnedChild,
    stopping: Arc<AtomicBool>,
    linked: Arc<AtomicBool>,
    linker: Option<std::thread::JoinHandle<()>>,
}

impl Fixture {
    pub(super) fn prepare() -> Self {
        let directory = Path::new("/run/gui-public/native-media");
        std::fs::create_dir(directory).unwrap();
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        let config = directory.join("pipewire.conf");
        std::fs::write(&config, CONFIG).unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut server = OwnedChild(
            owner_command("/usr/bin/pipewire")
                .arg("-c")
                .arg(config)
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                server.0.try_wait().unwrap().is_none(),
                "private null-output server exited before its owner socket was ready"
            );
            if std::fs::symlink_metadata("/run/user/62050/pipewire-0")
                .is_ok_and(|metadata| metadata.file_type().is_socket() && metadata.uid() == OWNER)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "private null-output server did not create its owner socket"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!Path::new(INPUT).exists());
        let generated = owner_command("/usr/bin/gst-launch-1.0")
            .args([
                "-q",
                "webmmux",
                "name=mux",
                "!",
                "filesink",
                &format!("location={INPUT}"),
                "videotestsrc",
                "num-buffers=900",
                "pattern=ball",
                "!",
                "video/x-raw,width=160,height=90,framerate=15/1",
                "!",
                "vp8enc",
                "deadline=1",
                "!",
                "queue",
                "!",
                "mux.video_0",
                "audiotestsrc",
                "num-buffers=2900",
                "wave=sine",
                "!",
                "audioconvert",
                "!",
                "vorbisenc",
                "!",
                "queue",
                "!",
                "mux.audio_0",
            ])
            .status()
            .unwrap();
        assert!(
            generated.success(),
            "generate private synthetic media without a capture device"
        );
        let metadata = std::fs::symlink_metadata(INPUT).unwrap();
        assert!(metadata.is_file() && metadata.uid() == OWNER && metadata.len() > 1024);
        let stopping = Arc::new(AtomicBool::new(false));
        let linked = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        let seen = linked.clone();
        let linker = std::thread::spawn(move || {
            let mut configured = HashSet::new();
            while !stop.load(Ordering::Acquire) {
                let result = owner_tool("/usr/bin/pw-dump")
                    .stdout(Stdio::piped())
                    .output();
                if let Ok(result) = result {
                    if let Ok(values) =
                        serde_json::from_slice::<Vec<serde_json::Value>>(&result.stdout)
                    {
                        for value in values {
                            let props = &value["info"]["props"];
                            if value["type"] != "PipeWire:Interface:Node"
                                || props["node.name"] != "pw-cat"
                                || props["media.class"] != "Stream/Output/Audio"
                            {
                                continue;
                            }
                            let Some(id) = value["id"].as_u64() else {
                                continue;
                            };
                            let Some(serial) = props["object.serial"].as_u64() else {
                                continue;
                            };
                            if configured.contains(&serial) {
                                continue;
                            }
                            let ports = owner_tool("/usr/bin/pw-cli").args([
                                "set-param", &id.to_string(), "PortConfig",
                                "{ direction = Output mode = dsp monitor = false control = false format = { mediaType = audio mediaSubtype = raw format = F32P rate = 48000 channels = 2 position = [ FL FR ] } }",
                            ]).status();
                            if !ports.is_ok_and(|status| status.success()) {
                                continue;
                            }
                            let mut success = true;
                            for channel in ["FL", "FR"] {
                                success &= owner_tool("/usr/bin/pw-link")
                                    .args([
                                        &format!("pw-cat:output_{channel}"),
                                        &format!("claw-private-null:playback_{channel}"),
                                    ])
                                    .status()
                                    .is_ok_and(|status| status.success());
                            }
                            if success {
                                configured.insert(serial);
                                seen.store(true, Ordering::Release);
                            }
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        Self {
            server,
            stopping,
            linked,
            linker: Some(linker),
        }
    }

    pub(super) async fn consume(
        &self,
        window: &str,
        session: &str,
        launcher: &ProcessIdentity,
        mut alive: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        use crate::clawd::authority::{
            self, Audience, AudienceSet, Binding, Decision, Issuance, Issuer, Presentation,
            Principal, Requirement, Subject, Uses,
        };
        let route = crate::clawd::routes::Command::SystemMediaPlayerControl.route();
        let caller_session = format!("media-fixture-{}", uuid::Uuid::new_v4().simple());
        let (_, grant) = authority::authority()
            .issue(Issuance {
                issuer: Issuer::AppSessionAuthority,
                principal: Principal::of_process(OWNER, launcher.pid() as u32).ok_or_else(
                    || "media controller process identity is unavailable".to_string(),
                )?,
                binding: Binding::ProcessTree,
                subject: Subject::session(&caller_session),
                audience: AudienceSet::one(Audience::SystemService),
                caps: CapSet::from_caps([
                    Cap::new(Verb::DESKTOP_MEDIA_OBSERVE, Scope::name("cosmic-player")),
                    Cap::new(Verb::DESKTOP_MEDIA_CONTROL, Scope::name("cosmic-player")),
                ]),
                lifetime: Duration::from_secs(90),
                uses: Uses::Unbounded,
                index_session: true,
            })
            .map_err(|error| error.to_string())?;
        let decision = Decision::for_test(
            grant,
            route.name,
            Audience::SystemService,
            Presentation {
                uid: OWNER,
                pid: launcher.pid() as u32,
                start_time_ticks: Some(launcher.start_ticks()),
                audience: Audience::SystemService,
                route: route.name,
                session_id: Some(caller_session.clone()),
            },
            None,
            &Requirement::RouteDerived,
        );
        let client = crate::clawd::client_identity::ClientIdentity::from_peer(
            crate::clawd::transport::PeerProcess {
                uid: OWNER,
                gid: OWNER,
                pid: launcher.pid() as u32,
                start_time_ticks: launcher.start_ticks(),
            },
        );
        let call = |action: &str| {
            let deadline = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64
                + 4500;
            crate::clawd::media_player::control(
                serde_json::json!({"session":caller_session,"action":action,"deadline_unix_ms":deadline}),
                &client,
                &decision,
            )
        };
        let deadline = Instant::now() + Duration::from_secs(25);
        let mut approved = false;
        loop {
            alive()?;
            if !approved {
                if let Some(request) = crate::approvals::list_pending_for_owner(Some(OWNER))
                    .into_iter()
                    .find(|request| {
                        request.verb == "fs.read"
                            && request.session.starts_with(&format!("gui-read-{window}-"))
                    })
                {
                    approve_native_resource(&request.id).await;
                    approved = true;
                }
            }
            if self.linked.load(Ordering::Acquire) {
                if let Ok(status) = call("status").await {
                    if status["status"] == "Playing" {
                        assert!(
                            approved,
                            "native media bytes were consumed before independent read consent"
                        );
                        assert_eq!(status["url"], format!("file://{INPUT}"));
                        assert!(status["length_micros"]
                            .as_i64()
                            .is_some_and(|length| length > 0));
                        break;
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err("actual native media, protected read or OS null-output stream did not become ready".into());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        for (action, expected) in [
            ("pause", "Paused"),
            ("play", "Playing"),
            ("toggle", "Paused"),
            ("toggle", "Playing"),
            ("stop", "Stopped"),
        ] {
            alive()?;
            assert_eq!(call(action).await?, serde_json::json!({"ok":true}));
            assert_eq!(call("status").await?["status"], expected);
        }
        for action in ["next", "previous"] {
            assert_eq!(call(action).await?, serde_json::json!({"ok":true}));
        }
        assert!(crate::clawd::gui::Manager::contains(session)?);
        authority::authority().revoke_session(&caller_session);
        assert!(
            call("status").await.is_err(),
            "revoked caller must not read another live GUI"
        );
        Ok(())
    }

    pub(super) fn verify_after_retirement(&self) {
        assert!(
            self.linked.load(Ordering::Acquire),
            "the actual OS audio helper never reached the private output"
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let dump = owner_tool("/usr/bin/pw-dump")
                .stdout(Stdio::piped())
                .output()
                .unwrap();
            assert!(dump.status.success());
            let values: Vec<serde_json::Value> = serde_json::from_slice(&dump.stdout).unwrap();
            if !values.iter().any(|value| {
                value["type"] == "PipeWire:Interface:Node"
                    && value["info"]["props"]["node.name"] == "pw-cat"
            }) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "retired GUI left an OS output stream in the private sound server"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(linker) = self.linker.take() {
            let _ = linker.join();
        }
        let _ = self.server.0.kill();
        let _ = self.server.0.wait();
    }
}

pub(super) async fn thumbnails(first_group: &std::fs::File) {
    let path = std::fs::read_link(format!("/proc/self/fd/{}", first_group.as_raw_fd())).unwrap();
    let root = path.parent().unwrap().parent().unwrap();
    let group = session_group(root, "41");
    const OUTPUT: &str = "/run/display-home/native-player-thumbnail.png";
    const SENTINEL: &[u8] = b"existing owner thumbnail stays unchanged before write consent";
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
        std::fs::write(OUTPUT, SENTINEL).unwrap();
    }
    let mut reports = Vec::new();
    for existing in [true, false] {
        if !existing {
            let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
            std::fs::remove_file(OUTPUT).unwrap();
        }
        let product = Product { case: 41, ..MEDIA };
        let (control, child_control) = bootstrap::pair().unwrap();
        let mut pam = spawn_pam("gui", "native-product", &group, Stdio::from(child_control));
        assert_eq!(
            status(&control, &pam, Instant::now() + Duration::from_secs(30)),
            *b"AUTH"
        );
        command(&control, b"CASE");
        command(&control, b"41..");
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
        assert_eq!(
            claw_display_control::KernelLogin::of(&launcher)
                .unwrap()
                .owner_uid,
            OWNER
        );
        let parent = launch_parent(&launcher, 41);
        command(&control, b"RUN!");
        approve_native_startup(&mut pam, product, &parent).await;
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
                pam.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "{}",
                diagnostic("thumbnail metadata consent")
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        approve_native_resource(&metadata.id).await;
        let deadline = Instant::now() + Duration::from_secs(25);
        let (session, retained, native) = loop {
            if let Some(session) = bound_gui_session(&parent, product) {
                let retained = retained_instance(&session);
                if let Some(native) = find_native(&retained, product) {
                    break (session, retained, native);
                }
            }
            assert!(
                pam.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "{}",
                diagnostic("actual thumbnail ELF")
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        let original = startup_authority(&session, &parent, product);
        let window = claw_display_control::Epoch(retained.instance.0).label();
        let runtime = Path::new("/run/cos/gui").join(&window);
        let mut read_approved = false;
        let mut write_approved = false;
        loop {
            if read_approved
                && write_approved
                && std::fs::read(OUTPUT).is_ok_and(|bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n"))
            {
                break;
            }
            native_process(&retained, &native, "actual thumbnail selected input/output");
            let requests = crate::approvals::list_pending_for_owner(Some(OWNER));
            if !read_approved {
                if let Some(request) = requests.iter().find(|request| {
                    request.verb == "fs.read"
                        && request.session.starts_with(&format!("gui-read-{window}-"))
                }) {
                    if existing {
                        assert_eq!(std::fs::read(OUTPUT).unwrap(), SENTINEL);
                    } else {
                        assert!(!Path::new(OUTPUT).exists());
                    }
                    approve_native_resource(&request.id).await;
                    read_approved = true;
                }
            }
            if !write_approved {
                if let Some(request) = requests.iter().find(|request| {
                    request.verb == "fs.write"
                        && request.scope == Scope::path(OUTPUT)
                        && request.operation_digest.is_some()
                }) {
                    assert!(
                        read_approved,
                        "thumbnail requested output before its selected media read"
                    );
                    if existing {
                        assert_eq!(std::fs::read(OUTPUT).unwrap(), SENTINEL);
                    } else {
                        assert!(!Path::new(OUTPUT).exists());
                    }
                    assert_eq!(startup_authority(&session, &parent, product), original);
                    approve_native_resource(&request.id).await;
                    write_approved = true;
                }
            }
            if read_approved
                && write_approved
                && std::fs::read(OUTPUT).is_ok_and(|bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n"))
            {
                break;
            }
            assert!(
                pam.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "{}",
                diagnostic("actual thumbnail PNG after separate content/write decisions")
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        command(&control, b"WAIT");
        assert_eq!(
            status(&control, &pam, Instant::now() + Duration::from_secs(20)),
            *b"PASS",
            "{}",
            diagnostic("thumbnail native outcome")
        );
        command(&control, b"CLSE");
        assert!(pam.wait().unwrap().success());
        assert!(observe().is_none(), "thumbnail mode created an unexpected GUI surface");
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
        let png = std::fs::read(OUTPUT).unwrap();
        assert!(png.len() > 100 && png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(&png[12..16], b"IHDR");
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        assert!(width > 0 && width <= 160 && height > 0 && height <= 90);
        assert_eq!(std::fs::metadata(OUTPUT).unwrap().uid(), OWNER);
        reports.push(serde_json::json!({"case":41,"existing":existing,"width":width,"height":height,
                    "selected_read":true,"selected_write":true,"actual_png":true,"native_exited":true,
                    "runtime_removed":true,"pam_exited":true}));
    }
    std::fs::write(
        "/run/gui-fixture/native-media-thumbnail.json",
        serde_json::to_vec_pretty(&reports).unwrap(),
    )
    .unwrap();
}
