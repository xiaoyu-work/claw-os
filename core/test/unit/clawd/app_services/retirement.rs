use super::*;
use crate::caps::manifest::McpLifecycle;

#[tokio::test]
async fn empty_service_retirement_is_idempotent() {
    let mut slot = ServiceSlot::new(McpLifecycle::Lazy);
    slot.retire().await.unwrap();
    slot.retire().await.unwrap();
    assert!(slot.runtime.is_none());
    assert!(slot.failures.is_empty());
}

#[test]
fn cleanup_errors_preserve_both_failures() {
    assert_eq!(
        join_cleanup_result(Err("broker failed".into()), Err("mount retained".into())),
        Err("broker failed; mount retained".into())
    );
    assert_eq!(
        join_cleanup_result(Ok(()), Err("mount retained".into())),
        Err("mount retained".into())
    );
}

#[cfg(target_os = "linux")]
mod process {
    use super::*;
    use crate::clawd::app_services::{
        AppServiceManager, CapacityLease, PreparedAppServiceCall, RuntimeSpec, ServiceKey,
        GLOBAL_SERVICE_LIMIT, OWNER_SERVICE_LIMIT,
    };
    use crate::extension_host::{identity::ExtensionIdentityPool, protocol::HostPurpose};
    use crate::provenance::{sign, PackageKind, TrustStore, TrustTier};
    use std::future::{poll_fn, Future};
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::task::Poll;
    use tokio::io::AsyncWriteExt;
    use tokio::sync::Mutex;

    fn mount_tmpfs(path: &std::ffi::CStr) {
        assert_eq!(
            unsafe {
                libc::mount(
                    c"tmpfs".as_ptr(),
                    path.as_ptr(),
                    c"tmpfs".as_ptr(),
                    libc::MS_NOSUID | libc::MS_NODEV,
                    c"mode=0755,size=1073741824".as_ptr().cast(),
                )
            },
            0,
            "private mount: {}",
            std::io::Error::last_os_error()
        );
    }

    fn own(path: &Path, uid: u32, gid: u32) {
        let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::chown(path.as_ptr(), uid, gid) }, 0);
    }

    fn executable(source: &Path, target: &Path) {
        std::fs::copy(source, target).unwrap();
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755)).unwrap();
        own(target, 0, 0);
    }

    struct CgroupRoot(PathBuf);

    impl CgroupRoot {
        fn create() -> Self {
            let path = Path::new("/sys/fs/cgroup")
                .join(format!("cos-retirement-{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for CgroupRoot {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir(&self.0) {
                eprintln!(
                    "private retirement cgroup {} remains: {error}",
                    self.0.display()
                );
            }
        }
    }

    fn install_app(
        root: &Path,
        key: &sign::SigningKeyFile,
        owner: u32,
        id: &str,
        lifecycle: McpLifecycle,
    ) -> RuntimeSpec {
        let dir = root.join(id);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("app.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 2,
                "id": id,
                "version": "0.1.0",
                "name": {"en": "Retirement fixture"},
                "summary": {"en": "Private service lifetime fixture"},
                "runtime": "python",
                "operations": {},
                "mcp": {
                    "transport": "stdio",
                    "entry": "server.py",
                    "lifecycle": lifecycle,
                    "tools": [{"name": "ping", "summary": {"en": "Ping"}, "args": [], "needs": []}]
                }
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("main.py"),
            "raise RuntimeError('not an operation fixture')\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("server.py"),
            "raise RuntimeError('no MCP calls expected')\n",
        )
        .unwrap();
        sign::sign_directory(
            &dir,
            &sign::SignRequest {
                kind: PackageKind::App,
                id: id.into(),
                version: "0.1.0".into(),
                manifest_schema: "cos.app/v2".into(),
                manifest_path: "app.json".into(),
                entrypoints: vec!["main.py".into(), "server.py".into()],
                resources: vec![],
            },
            key,
        )
        .unwrap();
        let app = crate::apps::find_verified_fresh(root, id).unwrap();
        let package = app.require_verified().unwrap();
        let record = crate::approvals::system_review::submit(
            owner,
            crate::approvals::system_review::ReviewKind::AppActivation,
            package,
            "private retirement fixture".into(),
        )
        .unwrap();
        crate::approvals::system_review::decide(owner, &record.id, true, Some(package)).unwrap();
        crate::approvals::system_review::consume(owner, &record.id, package).unwrap();
        RuntimeSpec {
            owner_uid: owner,
            app_id: id.into(),
            package: crate::provenance::runtime::PackageRef::of(package),
            lifecycle,
        }
    }

    async fn insert_slot(
        manager: &Arc<AppServiceManager>,
        spec: &RuntimeSpec,
    ) -> Arc<Mutex<ServiceSlot>> {
        let slot = Arc::new(Mutex::new(ServiceSlot::new(spec.lifecycle)));
        manager.slots.lock().await.insert(
            ServiceKey {
                owner_uid: spec.owner_uid,
                app_id: spec.app_id.clone(),
            },
            slot.clone(),
        );
        slot
    }

    fn held_capacity(manager: &AppServiceManager, owner: u32, count: usize) {
        assert_eq!(
            manager.capacity.available_permits(),
            GLOBAL_SERVICE_LIMIT - count
        );
        assert_eq!(
            manager
                .owner_counts
                .lock()
                .unwrap()
                .get(&owner)
                .copied()
                .unwrap_or(0),
            count
        );
    }

    fn owner_is_fenced(pool: &Arc<ExtensionIdentityPool>, owner: u32) {
        for purpose in [HostPurpose::Task, HostPurpose::AppService] {
            let error = pool.acquire(owner, purpose).unwrap_err();
            assert!(error.contains("cleanup"), "{error}");
        }
        pool.acquire(owner + 1, HostPurpose::Task)
            .unwrap()
            .release()
            .unwrap();
    }

    fn has_accepted_connection(path: &Path) -> bool {
        for entry in std::fs::read_dir("/proc/self/fd").unwrap() {
            let fd: i32 = entry
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .parse()
                .unwrap();
            let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
            if duplicate < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EBADF)
                );
                continue;
            }
            let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(duplicate) };
            let local = stream.local_addr();
            if local
                .as_ref()
                .ok()
                .and_then(|address| address.as_pathname())
                == Some(path)
                && stream.peer_addr().is_ok()
            {
                return true;
            }
        }
        false
    }

    #[tokio::test]
    #[ignore = "requires Root, private mount namespace, cgroup v2 and COS_SERVICE_RETIREMENT_HOST"]
    async fn service_retirement_keeps_custody_until_cleanup_succeeds() {
        let _lock = crate::test_env::lock_env();
        let _tracing = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::INFO)
                .with_ansi(false)
                .with_writer(std::io::stderr)
                .finish(),
        );
        assert_eq!(unsafe { libc::geteuid() }, 0);
        assert_ne!(
            std::fs::read_link("/proc/self/ns/mnt").unwrap(),
            std::fs::read_link("/proc/1/ns/mnt").unwrap(),
            "this fixture replaces only private runtime mounts"
        );
        let owner_uid = std::fs::metadata(env!("CARGO_MANIFEST_DIR")).unwrap().uid();
        assert_ne!(owner_uid, 0);
        let owner = crate::agentd::spawn::resolve_identity(owner_uid).unwrap();
        let source = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        own(source.path(), owner.uid, owner.gid);
        let data = source.path().join("owner-data");
        let host_source = PathBuf::from(std::env::var("COS_SERVICE_RETIREMENT_HOST").unwrap());
        mount_tmpfs(c"/run");
        mount_tmpfs(c"/var/lib");
        std::fs::create_dir("/run/lock").unwrap();
        std::fs::set_permissions("/run/lock", std::fs::Permissions::from_mode(0o1777)).unwrap();
        let root = Path::new("/run/sr");
        std::fs::create_dir(root).unwrap();
        for name in ["apps", "trust", "runtime", "state"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        let host = root.join("claw-extension-host");
        executable(&host_source, &host);
        let group = CgroupRoot::create();
        let _environment = [
            crate::test_env::TestEnvVarGuard::set("COS_EXTENSION_HOST_BIN", &host),
            crate::test_env::TestEnvVarGuard::set("CLAWD_EXTENSION_CGROUP_ROOT", &group.0),
            crate::test_env::TestEnvVarGuard::set("COS_EXTENSION_EXEC_GROUP", "nogroup"),
            crate::test_env::TestEnvVarGuard::set("COS_RUNTIME_DIR", root.join("runtime")),
            crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.join("state")),
            crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", root.join("caps")),
            crate::test_env::TestEnvVarGuard::set("COS_USER_DATA_DIR", &data),
            crate::test_env::TestEnvVarGuard::set("COS_APPS_DIR", root.join("apps")),
        ];
        let key = sign::SigningKeyFile::generate(None).unwrap();
        std::fs::write(
            root.join("trust/fixture.json"),
            serde_json::to_vec(&key.trust_entry(&[PackageKind::App])).unwrap(),
        )
        .unwrap();
        let roots = vec![crate::provenance::trust::TrustRootSpec {
            path: root.join("trust"),
            tier: TrustTier::System,
            allowed_uids: vec![0],
            domain: crate::provenance::state::TrustDomain::System,
        }];
        crate::test_env::record_trust_state(&roots);
        let trust = TrustStore::load_roots(&roots);
        assert!(trust.diagnostics().is_empty(), "{:?}", trust.diagnostics());
        crate::provenance::set_trust_store_for_roots(trust, roots);

        let primary = root.join("clawd.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&primary).unwrap();
        own(&primary, 0, owner.gid);
        std::fs::set_permissions(&primary, std::fs::Permissions::from_mode(0o660)).unwrap();
        let state = crate::clawd::state::DaemonState::new().unwrap();
        let broker = crate::agentd::supervisor::BrokerContext::new(
            state,
            crate::clawd::transport::limits::Admission::new(Default::default()),
            primary,
        )
        .unwrap();
        let pool = ExtensionIdentityPool::for_test_with_quarantine(
            broker.isolated_execution_gid,
            root.join("quarantine"),
        )
        .unwrap();
        let manager = AppServiceManager::new(broker.with_test_identity_pool(pool.clone()));
        let spec = install_app(
            &root.join("apps"),
            &key,
            owner.uid,
            "retirement-lazy",
            McpLifecycle::Lazy,
        );
        let slot = insert_slot(&manager, &spec).await;
        let canary = data.join("apps").join(&spec.app_id).join("canary");
        {
            let mut held = slot.lock().await;
            manager.start_runtime(&spec, &mut held).await.unwrap();
            {
                let _identity =
                    crate::clawd::client_identity::FsIdentityGuard::enter(owner.uid).unwrap();
                std::fs::write(&canary, b"persistent owner data").unwrap();
            }
            let runtime = held.runtime.as_ref().unwrap();
            assert!(!runtime.retiring);
            runtime.host.mount_private_tmp_test_child().unwrap();
            let broker_socket = runtime.host.paths.broker_socket.clone();
            let mut pending_request = tokio::net::UnixStream::connect(&broker_socket)
                .await
                .unwrap();
            pending_request.write_all(b"C").await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while !has_accepted_connection(&broker_socket) {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("the real broker must accept the partial request");
            held_capacity(&manager, owner.uid, 1);
            {
                let mut retirement = std::pin::pin!(held.retire());
                let polled = poll_fn(|cx| Poll::Ready(retirement.as_mut().poll(cx))).await;
                assert!(
                    polled.is_pending(),
                    "live broker retirement must yield before completion"
                );
            }
            assert!(held.runtime.as_ref().unwrap().retiring);
            assert!(held.runtime.as_ref().unwrap().client.is_none());
            owner_is_fenced(&pool, owner.uid);
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    match tokio::net::UnixStream::connect(&broker_socket).await {
                        Ok(connection) => drop(connection),
                        Err(error) => {
                            assert!(matches!(
                                error.raw_os_error(),
                                Some(libc::ECONNREFUSED | libc::ENOENT)
                            ));
                            break;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("retirement must close broker admission");
            assert!(!held
                .runtime
                .as_ref()
                .unwrap()
                .broker_task
                .as_ref()
                .unwrap()
                .is_finished());
            let error = held.retire().await.unwrap_err();
            assert!(error.contains("requests are still draining"), "{error}");
            assert!(held.runtime.as_ref().unwrap().broker_task.is_some());
            held_capacity(&manager, owner.uid, 1);
            drop(pending_request);
            let error = held.retire().await.unwrap_err();
            assert!(error.contains("incomplete"), "{error}");
            let runtime = held.runtime.as_mut().unwrap();
            assert!(runtime.host.child.try_wait().unwrap().is_some());
            assert!(runtime.broker_task.is_none());
            assert_eq!(runtime.identity.identity().uid, runtime.extension_uid);
            held_capacity(&manager, owner.uid, 1);
            let refused = manager.start_runtime(&spec, &mut held).await.unwrap_err();
            assert!(!refused.counts_as_failure);
            assert!(held.failures.is_empty());
        }
        let canary_identity = std::fs::metadata(&canary).unwrap();
        let context = crate::agent::tools::app_gateway::McpCallContext {
            wire_version: crate::agent::tools::app_gateway::CALL_CONTEXT_WIRE_VERSION,
            call_id: "retirement-call".into(),
            trace_id: "retirement-trace".into(),
            deadline_unix_ms: Some(crate::agentd::grant::now_ms() + 60_000),
            session_id: Some("retirement-session".into()),
            task_id: Some("retirement-task".into()),
            caller: crate::agent::tools::app_gateway::McpPrincipal {
                kind: crate::agent::tools::app_gateway::McpPrincipalKind::SystemAgent,
                id: "retirement-session".into(),
                owner_uid: owner.uid,
            },
        };
        let error = manager
            .call(PreparedAppServiceCall {
                owner_uid: owner.uid,
                app_id: spec.app_id.clone(),
                tool: "ping".into(),
                arguments: serde_json::json!({}),
                capability_generation: "fixture".into(),
                package: spec.package.clone(),
                caps: crate::caps::CapSet::from_caps([]),
                placement: crate::agent::tools::cos_apps_session::CallPlacement::Reusable,
                authorized_mounts: vec![],
                lifecycle: spec.lifecycle,
                deadline_ms: context.deadline_unix_ms.unwrap(),
                context,
            })
            .await
            .unwrap_err();
        assert!(error.message.contains("retirement"), "{error:?}");
        manager.sweep().await;
        {
            let held = slot.lock().await;
            assert!(held.runtime.as_ref().unwrap().retiring);
            assert!(
                held.failures.is_empty(),
                "cleanup retries are not new Host crashes"
            );
        }
        let mut additional_capacity: Vec<CapacityLease> = Vec::new();
        for _ in 1..OWNER_SERVICE_LIMIT {
            additional_capacity.push(manager.acquire_capacity(owner.uid).unwrap());
        }
        let error = manager.evict_for_capacity(owner.uid).await.unwrap_err();
        assert!(error.contains("retirement"), "{error}");
        held_capacity(&manager, owner.uid, OWNER_SERVICE_LIMIT);
        let other_capacity = (0..OWNER_SERVICE_LIMIT)
            .map(|_| manager.acquire_capacity(owner.uid + 1).unwrap())
            .collect::<Vec<_>>();
        let error = manager.evict_for_capacity(owner.uid + 2).await.unwrap_err();
        assert!(error.contains("retirement"), "{error}");
        assert!(!error.contains(&spec.app_id), "{error}");
        assert!(!error.contains(&owner.uid.to_string()), "{error}");
        drop(other_capacity);
        drop(additional_capacity);
        assert!(manager.stop_all().await.unwrap_err().contains("retirement"));
        {
            let mut held = slot.lock().await;
            held.runtime
                .as_ref()
                .unwrap()
                .host
                .unmount_private_tmp_test_child()
                .unwrap();
            held.retire().await.unwrap();
            held.retire().await.unwrap();
            assert!(held.runtime.is_none());
        }
        held_capacity(&manager, owner.uid, 0);
        pool.acquire(owner.uid, HostPurpose::Task)
            .unwrap()
            .release()
            .unwrap();

        {
            let mut held = slot.lock().await;
            {
                let mut startup = std::pin::pin!(manager.start_runtime(&spec, &mut held));
                let polled = poll_fn(|cx| Poll::Ready(startup.as_mut().poll(cx))).await;
                assert!(polled.is_pending());
            }
            let runtime = held.runtime.as_ref().unwrap();
            assert!(
                runtime.retiring,
                "interrupted startup cannot become a reusable Host"
            );
            assert!(runtime.client.is_none());
            owner_is_fenced(&pool, owner.uid);
            held_capacity(&manager, owner.uid, 1);
            held.retire().await.unwrap();
        }
        held_capacity(&manager, owner.uid, 0);

        {
            let mut held = slot.lock().await;
            manager.start_runtime(&spec, &mut held).await.unwrap();
            let execution_uid = held.runtime.as_ref().unwrap().extension_uid;
            let execution_gid = manager.broker.isolated_execution_gid;
            let mut command = tokio::process::Command::new("/usr/bin/sleep");
            command.arg("60").kill_on_drop(true);
            unsafe {
                command.pre_exec(move || {
                    if libc::setgroups(0, std::ptr::null()) != 0
                        || libc::setresgid(execution_gid, execution_gid, execution_gid) != 0
                        || libc::setresuid(execution_uid, execution_uid, execution_uid) != 0
                        || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut survivor = command.spawn().unwrap();
            let error = held.retire().await.unwrap_err();
            assert!(error.contains("still owns a process"), "{error}");
            owner_is_fenced(&pool, owner.uid);
            held_capacity(&manager, owner.uid, 1);
            survivor.kill().await.unwrap();
            held.retire().await.unwrap();
        }
        held_capacity(&manager, owner.uid, 0);

        let always = install_app(
            &root.join("apps"),
            &key,
            owner.uid,
            "retirement-always",
            McpLifecycle::AlwaysOn,
        );
        let always_slot = insert_slot(&manager, &always).await;
        {
            let mut held = always_slot.lock().await;
            manager.start_runtime(&always, &mut held).await.unwrap();
            held.runtime
                .as_ref()
                .unwrap()
                .host
                .mount_private_tmp_test_child()
                .unwrap();
            assert!(held.retire().await.is_err());
        }
        manager.warm_always_on_for_owner(owner.uid).await;
        {
            let mut held = always_slot.lock().await;
            assert!(held.runtime.as_ref().unwrap().retiring);
            assert!(held.failures.is_empty());
            held.runtime
                .as_ref()
                .unwrap()
                .host
                .unmount_private_tmp_test_child()
                .unwrap();
            held.retire().await.unwrap();
        }
        manager.stop_all().await.unwrap();
        held_capacity(&manager, owner.uid, 0);
        assert_eq!(std::fs::read(&canary).unwrap(), b"persistent owner data");
        let after = std::fs::metadata(&canary).unwrap();
        assert_eq!(after.ino(), canary_identity.ino());
        assert_eq!((after.uid(), after.gid()), (owner.uid, owner.gid));
        {
            let mut held = slot.lock().await;
            manager.start_runtime(&spec, &mut held).await.unwrap();
            let runtime = held.runtime.as_mut().unwrap();
            let old_broker = runtime.broker_task.take().unwrap();
            old_broker.abort();
            assert!(old_broker.await.unwrap_err().is_cancelled());
            runtime.broker_task = Some(tokio::spawn(async {
                panic!("private broker-task failure fixture");
            }));
            tokio::task::yield_now().await;
            let error = held.retire().await.unwrap_err();
            assert!(error.contains("broker task failed"), "{error}");
            assert!(held
                .runtime
                .as_mut()
                .unwrap()
                .host
                .child
                .try_wait()
                .unwrap()
                .is_some());
            owner_is_fenced(&pool, owner.uid);
            assert!(held
                .retire()
                .await
                .unwrap_err()
                .contains("broker recovery is required"));
            assert!(held.runtime.as_ref().unwrap().broker_failure.is_some());
        }
        held_capacity(&manager, owner.uid, 1);
        assert!(manager.stop_all().await.is_err());
        assert_eq!(
            std::fs::read_dir(&group.0)
                .unwrap()
                .map(Result::unwrap)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("cos-extension-"))
                .count(),
            0
        );
    }
}
