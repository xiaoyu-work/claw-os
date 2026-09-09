struct NotifyWorker {
    process: tokio::process::Child,
    writer: tokio::process::ChildStdin,
    reader: Lines,
    session: String,
    grant: crate::clawd::authority::GrantView,
    relay_grant: crate::clawd::authority::GrantView,
    resources: crate::worker::LaunchResources,
}

impl NotifyWorker {
    fn grant_for(
        pid: u32,
        session: &str,
        app: &str,
        caps: crate::caps::CapSet,
    ) -> crate::clawd::authority::GrantView {
        authority()
            .issue(Issuance {
                issuer: Issuer::AppSessionAuthority,
                principal: Principal::of_process(unsafe { libc::geteuid() }, pid).unwrap(),
                binding: Binding::ProcessTree,
                subject: Subject::session(session)
                    .with_app(Some(app.into()))
                    .with_task(Some("authenticated-notify-task".into())),
                audience: AudienceSet::one(Audience::SystemService),
                caps,
                lifetime: Duration::from_secs(120),
                uses: Uses::Unbounded,
                index_session: true,
            })
            .unwrap()
            .1
    }

    async fn start(
        package: &crate::provenance::VerifiedPackage,
        data: &std::path::Path,
        root: &std::path::Path,
        caps: crate::caps::CapSet,
    ) -> Self {
        assert_eq!(package.id(), NOTIFY_APP);
        let session = format!("notify-{}", uuid::Uuid::new_v4().simple());
        let binding = package.bind_for_launch(package.entrypoints()).unwrap();
        let uid = unsafe { libc::geteuid() };
        crate::provenance::runtime::register(uid, &session, package);
        let sdk = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../claw-os-sdk/python/src")
            .canonicalize()
            .unwrap();
        let slot = crate::worker::relay_slot();
        // The endpoint can route either declared family; live per-call grants
        // below independently govern send and list at the real OS provider.
        let declared = crate::caps::CapSet::from_caps([
            Cap::unscoped(Verb::UI_NOTIFY),
            Cap::new(Verb::DATA_INBOX_READ, Scope::Wild),
        ]);
        let mut policy = derive::app_session(derive::AppSessionInput {
            app_id: package.id(),
            app_dir: binding.dir(),
            program: PathBuf::from("/usr/bin/python3"),
            argv: vec![binding.dir().join("server.py").to_str().unwrap().into()],
            caps: &declared,
            authorized_mounts: &[],
            lifetime: derive::SessionLifetime::Reusable,
            session_id: &session,
            data_dir: data.to_str().unwrap(),
            apps_dir: root.to_str().unwrap(),
            extra_env: BTreeMap::from([
                ("PYTHONPATH".into(), sdk.to_str().unwrap().into()),
                ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
                (
                    "COS_APP_MANIFEST".into(),
                    binding.dir().join("app.json").to_str().unwrap().into(),
                ),
            ]),
            package_identity: Some(binding.dir_identity()),
            pinned_entries: binding.entries(),
            transports: &[],
        })
        .unwrap();
        assert_eq!(policy.workdir, data.join("apps/notify"));
        policy.env.remove("CLAW_COS_BIN");
        policy
            .env
            .insert("TMPDIR".into(), policy.workdir.to_str().unwrap().into());
        policy
            .mounts
            .retain(|mount| mount.target != std::path::Path::new("/usr/local/bin/cos"));
        policy.mounts.push(Mount::read_only(
            input("COS_NOTIFICATIONS_COS"),
            "/usr/local/bin/cos",
            MountClass::Runtime,
        ));
        policy
            .mounts
            .push(Mount::read_only(&sdk, &sdk, MountClass::Runtime));
        assert!(policy
            .mounts
            .iter()
            .all(|mount| mount.class != MountClass::Display));
        assert!(policy.mounts.iter().all(|mount| mount.source != data));
        assert!(!policy.env.contains_key("DBUS_SESSION_BUS_ADDRESS"));
        assert_eq!(policy.network.as_str(), "denied");
        assert_eq!(policy.seccomp.as_str(), "strict");
        let prepared = crate::worker::prepare(
            &WorkerLaunch::new(policy).with_authority(
                BrokerAuthority::new(&session, Some(package.id().into()), declared, slot.clone())
                    .with_package(Some(binding.package_ref())),
            ),
        )
        .unwrap();
        let mut process = tokio::process::Command::from(prepared.command)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let writer = process.stdin.take().unwrap();
        let reader = tokio::io::BufReader::new(process.stdout.take().unwrap()).lines();
        crate::provenance::runtime::bind_process(uid, &session, process.id().unwrap());
        let grant = Self::grant_for(process.id().unwrap(), &session, package.id(), caps);
        let (relay, relay_grant) = authority()
            .issue(Issuance {
                issuer: Issuer::AppSessionAuthority,
                principal: Principal::of_process(uid, std::process::id()).unwrap(),
                binding: Binding::Process,
                subject: Subject::session(&session).with_app(Some(package.id().into())),
                audience: AudienceSet::one(Audience::AppRelay),
                caps: crate::caps::CapSet::new(),
                lifetime: Duration::from_secs(120),
                uses: Uses::Unbounded,
                index_session: false,
            })
            .unwrap();
        crate::worker::install_relay(&slot, Some(relay.into_wire()));
        let mut worker = Self {
            process,
            writer,
            reader,
            session,
            grant,
            relay_grant,
            resources: prepared.resources,
        };
        rpc(
            &mut worker.writer,
            &mut worker.reader,
            1,
            "initialize",
            json!({
                "protocolVersion":"2025-06-18","capabilities":{},
                "clientInfo":{"name":"signed-notify-fixture","version":"1"},
            }),
        )
        .await;
        worker
            .writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        worker
    }

    fn set_caps(&mut self, app: &str, caps: crate::caps::CapSet) {
        authority().revoke(self.grant.id);
        self.grant = Self::grant_for(self.process.id().unwrap(), &self.session, app, caps);
    }

    async fn call(&mut self, id: u64, name: &str, args: Value) -> Value {
        rpc(
            &mut self.writer,
            &mut self.reader,
            id,
            "tools/call",
            call(name, args),
        )
        .await
    }

    async fn stop(mut self) {
        self.process.kill().await.unwrap();
        self.process.wait().await.unwrap();
        authority().revoke(self.grant.id);
        authority().revoke(self.relay_grant.id);
        crate::provenance::runtime::deregister(unsafe { libc::geteuid() }, &self.session);
        drop(self.resources);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires staged Notify package, cos and the existing native presentation/delivery fixtures"]
async fn notifications_signed_notify_worker_persists_and_shares_native_delivery() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    let _lock = crate::test_env::lock_env();
    let source = input("COS_NOTIFY_PACKAGE");
    let presenter = input("COS_NOTIFICATIONS_PRESENTER");
    let bridge = input("COS_NOTIFICATIONS_DELIVERY");
    let root = tempfile::Builder::new()
        .prefix("nl-")
        .tempdir_in(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../build"))
        .unwrap();
    let root = root.path().canonicalize().unwrap();
    let home = root.join("home");
    let data = root.join("data");
    let app = root.join("apps/notify");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&app).unwrap();
    std::fs::create_dir_all(data.join("apps/notify")).unwrap();
    let _home = crate::test_env::TestEnvVarGuard::set("HOME", &home);
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", &data);
    let _runtime = crate::test_env::TestEnvVarGuard::set("COS_RUNTIME_DIR", &root);
    let _xdg = crate::test_env::TestEnvVarGuard::set("XDG_RUNTIME_DIR", &root);
    let _instances =
        crate::test_env::TestEnvVarGuard::set("COS_PROVENANCE_RUNTIME_DIR", root.join("instances"));
    let _proxy = crate::test_env::TestEnvVarGuard::remove(
        crate::extension_host::protocol::BROKER_SOCKET_ENV,
    );
    let files = ["app.json", "main.py", "server.py", "client.py"];
    for file in files {
        std::fs::copy(source.join(file), app.join(file)).unwrap();
    }
    crate::test_env::sign_test_package(&app, crate::provenance::PackageKind::App, NOTIFY_APP);
    let trust = crate::provenance::trust_store();
    let package = crate::provenance::verify::verify_package(
        &app,
        &crate::provenance::VerifyOptions::new(crate::provenance::PackageKind::App)
            .expect_id(NOTIFY_APP),
        &trust,
    )
    .unwrap();
    assert!(matches!(
        package.source(),
        crate::provenance::TrustSource::Publisher { .. }
    ));
    let manifest =
        crate::caps::manifest::Manifest::from_json(&package.manifest_text().unwrap()).unwrap();
    assert_eq!(manifest.id, NOTIFY_APP);
    let paths = crate::caps::args::PathContext {
        home: home.clone(),
        cwd: None,
    };
    let read_caps = crate::caps::CapSet::from_caps(
        manifest
            .resolve_mcp_tool_call("notify.list", &BTreeMap::new(), &paths)
            .unwrap()
            .needs
            .into_iter()
            .flatten(),
    );
    let send_caps = crate::caps::CapSet::from_caps(
        manifest
            .resolve_mcp_tool_call(
                "notify.send",
                &BTreeMap::from([("message".into(), json!("Fixture"))]),
                &paths,
            )
            .unwrap()
            .needs
            .into_iter()
            .flatten(),
    );
    assert_eq!(
        read_caps,
        crate::caps::CapSet::from_caps([Cap::new(Verb::DATA_INBOX_READ, Scope::Wild)])
    );
    assert_eq!(
        send_caps,
        crate::caps::CapSet::from_caps([Cap::unscoped(Verb::UI_NOTIFY)])
    );
    let histories = [
        data.join("notifications.json"),
        data.join("apps/notify/notifications.json"),
    ];
    for (index, file) in histories.iter().enumerate() {
        std::fs::write(
            file,
            if index == 0 {
                b"{malformed history".as_slice()
            } else {
                b"[{\"old_schema\":\"never replay\"}]"
            },
        )
        .unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o400)).unwrap();
    }
    let snapshot = |file: &PathBuf| {
        let meta = std::fs::metadata(file).unwrap();
        (
            meta.ino(),
            meta.uid(),
            meta.gid(),
            meta.mode(),
            meta.len(),
            meta.mtime(),
            meta.atime(),
        )
    };
    let before: Vec<_> = histories.iter().map(snapshot).collect();
    let uid = unsafe { libc::geteuid() };
    let calls = Arc::new(AtomicUsize::new(0));
    let mut server = notification_broker(&root, uid, calls.clone()).await;
    let mut worker = NotifyWorker::start(&package, &data, &root, read_caps.clone()).await;
    let empty = result(worker.call(2, "notify.list", json!({})).await);
    assert_eq!(empty, json!({"notifications":[],"total":0}));
    assert_eq!(
        worker
            .call(3, "notify.send", json!({"message":"No send grant"}))
            .await["result"]["isError"],
        true
    );
    let count = calls.load(Ordering::SeqCst);
    for (tool, args) in [
        ("notify.send", json!({"message":""})),
        ("notify.send", json!({"message":"x".repeat(4001)})),
        ("notify.send", json!({"message":"Hi","urgent":1})),
        ("notify.list", json!({"limit":true})),
        ("notify.list", json!({"limit":101})),
        ("notify.list", json!({"source":SOURCE})),
        ("notify.send", json!({"message":"Hi","owner_uid":0})),
        ("notify.send", json!({"message":"Hi","confirm":true})),
    ] {
        assert_eq!(worker.call(4, tool, args).await["result"]["isError"], true);
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        count,
        "invalid input reached policy/provider"
    );
    let service = notifications::open_default().unwrap();
    for (owner, source) in [(uid + 1, NOTIFY_SOURCE), (uid, SOURCE), (uid, "agent")] {
        service
            .publish(
                owner,
                NotificationDraft::new(
                    source,
                    "fixture",
                    Severity::Info,
                    "Private",
                    "Other source/owner",
                )
                .activity(),
            )
            .unwrap();
    }
    assert_eq!(
        result(worker.call(5, "notify.list", json!({})).await)["total"],
        0
    );
    worker.set_caps(APP, read_caps.clone());
    assert_eq!(
        worker.call(6, "notify.list", json!({})).await["result"]["isError"],
        true
    );
    worker.set_caps(NOTIFY_APP, send_caps.clone());
    assert_eq!(
        worker.call(7, "notify.list", json!({})).await["result"]["isError"],
        true
    );

    let address = format!("unix:path={}/bus", root.display());
    let mut bus = tokio::process::Command::new("dbus-daemon")
        .args([
            "--session",
            "--nofork",
            "--nopidfile",
            "--nosyslog",
            "--print-address=1",
        ])
        .arg(format!("--address={address}"))
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut bus_lines = tokio::io::BufReader::new(bus.stdout.take().unwrap()).lines();
    assert!(line(&mut bus_lines).await.starts_with(&address));
    let start = |binary: &PathBuf| {
        let mut command = tokio::process::Command::new(binary);
        command
            .env_clear()
            .env("DBUS_SESSION_BUS_ADDRESS", &address)
            .env("HOME", &home)
            .env("COS_RUNTIME_DIR", &root)
            .env("TMPDIR", &root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        command
    };
    let mut ui = start(&presenter).spawn().unwrap();
    let mut ui_input = ui.stdin.take().unwrap();
    let mut ui_output = tokio::io::BufReader::new(ui.stdout.take().unwrap()).lines();
    assert_eq!(line(&mut ui_output).await, "ready");
    let mut delivery = start(&bridge).arg(&presenter).spawn().unwrap();
    let mut delivery_output = tokio::io::BufReader::new(delivery.stdout.take().unwrap()).lines();
    assert_eq!(line(&mut delivery_output).await, "ready");
    let first = result(
        worker
            .call(8, "notify.send", json!({"message":"<b>Python first</b>"}))
            .await,
    );
    let shown = rendered(&mut ui_output).await;
    assert_eq!(shown["body"], "&lt;b&gt;Python first&lt;/b&gt;");
    assert_eq!(shown["app_name"], "Notifications");
    assert_eq!(shown["expire_timeout"], -1);
    let first_id = first["id"].as_str().unwrap().to_owned();
    let record = service.get(uid, &first_id).unwrap();
    assert_eq!(record.body, "<b>Python first</b>");
    assert_eq!(record.source, NOTIFY_SOURCE);
    assert_eq!(record.session_id.as_deref(), Some(worker.session.as_str()));
    assert_eq!(record.task_id.as_deref(), Some("authenticated-notify-task"));
    assert_eq!(record.state, NotificationState::Unread);
    ui_input
        .write_all(format!("{}\n", json!({"action":"ack","id":shown["id"]})).as_bytes())
        .await
        .unwrap();
    wait_state(&service, uid, &first_id, NotificationState::Acknowledged).await;
    let second = result(
        worker
            .call(
                9,
                "notify.send",
                json!({"message":"Second\n界","urgent":true}),
            )
            .await,
    );
    let shown = rendered(&mut ui_output).await;
    let second_id = second["id"].as_str().unwrap().to_owned();
    assert_eq!(
        service.get(uid, &second_id).unwrap().severity,
        Severity::Warning
    );
    assert_eq!(
        service.get(uid, &second_id).unwrap().state,
        NotificationState::Unread
    );
    worker.set_caps(NOTIFY_APP, read_caps.clone());
    let page = result(worker.call(10, "notify.list", json!({"limit":1})).await);
    assert_eq!(page["total"], 2);
    assert_eq!(page["notifications"].as_array().unwrap().len(), 1);
    assert_eq!(page["notifications"][0]["id"], second_id);
    assert_eq!(page["notifications"][0]["read"], false);
    ui_input
        .write_all(format!("{}\n", json!({"action":"dismiss","id":shown["id"]})).as_bytes())
        .await
        .unwrap();
    wait_state(&service, uid, &second_id, NotificationState::Dismissed).await;
    for process in [&mut delivery, &mut ui, &mut bus] {
        process.kill().await.unwrap();
        process.wait().await.unwrap();
    }
    worker.stop().await;
    drop(service);
    server.abort();
    let _ = server.await;
    std::fs::remove_file(root.join("clawd.sock")).unwrap();
    server = notification_broker(&root, uid, calls.clone()).await;
    worker = NotifyWorker::start(&package, &data, &root, read_caps.clone()).await;
    let restarted = result(worker.call(11, "notify.list", json!({})).await);
    assert_eq!(restarted["total"], 2);
    assert_eq!(restarted["notifications"][0]["id"], second_id);
    assert_eq!(restarted["notifications"][0]["state"], "dismissed");
    assert_eq!(restarted["notifications"][1]["id"], first_id);
    assert_eq!(restarted["notifications"][1]["state"], "acknowledged");
    assert_eq!(
        restarted["notifications"][1]["timestamp"],
        first["timestamp"]
    );
    {
        let _offline = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.join("offline"));
        std::fs::create_dir_all(crate::paths::notifications_db_path()).unwrap();
        let denied = worker.call(12, "notify.list", json!({})).await;
        assert_eq!(denied["result"]["isError"], true);
        assert!(denied["result"]["structuredContent"]["code"].is_string());
        worker.set_caps(NOTIFY_APP, send_caps);
        assert_eq!(
            worker
                .call(13, "notify.send", json!({"message":"Must not replay"}))
                .await["result"]["isError"],
            true
        );
    }
    worker.set_caps(NOTIFY_APP, read_caps);
    assert_eq!(
        result(worker.call(14, "notify.list", json!({})).await),
        restarted
    );
    worker.stop().await;
    server.abort();
    let _ = server.await;
    for (file, previous) in histories.iter().zip(before) {
        assert_eq!(
            snapshot(file),
            previous,
            "legacy history was inspected or changed"
        );
    }
    assert!(!data.join("apps/notify/notifications.json.lock").exists());
    assert!(!data.join("apps/notify/.cos-state-version").exists());
}
