use super::*;

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::clawd::app_sessions::task_host::tests::{parent, Fixture, HostProcess};
use crate::clawd::protocol::BrokerErrorKind;

const APP: &str = "session-app";
const MANIFEST: &str = r#"{
  "id":"session-app","version":"0.1.0","name":"Session fixture",
  "session":{
    "entry":"server.py","transport":"stdio",
    "tools":[
      {"name":"session.readwrite","summary":"Read and write",
       "args":[{"name":"path","kind":"path","required":true},
               {"name":"format","kind":"text","default":"plain"}],
       "needs":[
         {"verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},"why":"read"},
         {"verb":"fs.write","scope":{"kind":"from-arg","arg":"path"},"why":"write"}]},
      {"name":"session.default","summary":"Default path",
       "args":[{"name":"path","kind":"path","default":"~/state.txt"},
               {"name":"enabled","kind":"bool"}],
       "needs":[{"verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},"why":"read"}]}
    ]
  }
}"#;

fn input(path: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([("path".to_string(), json!(path))])
}

fn specification(app: &App) -> TaskHostSession<'_> {
    TaskHostSession {
        app_id: &app.manifest.id,
        package_digest: app.require_verified().unwrap().content_digest(),
    }
}

fn parent_caps() -> CapSet {
    CapSet::from_caps([
        Cap::new(Verb::AGENT_INVOKE, Scope::name(APP)),
        Cap::new(Verb::FS_READ, Scope::path("/home/stateful/**")),
        Cap::new(Verb::FS_WRITE, Scope::path("/home/stateful/**")),
        Cap::new(Verb::SYS_PACKAGE, Scope::name("nano")),
    ])
}

#[test]
fn session_preparation_authenticates_nnp_host_without_issuing_authority() {
    let mut fixture = Fixture::new();
    let app = fixture.signed_app_with_entries(MANIFEST, &["server.py"]);
    let host = HostProcess::spawn(true);
    let parent = parent(parent_caps());
    let before = authority::authority().len();
    prepare_session_for_task_host(&host.client, &parent, &specification(&app)).unwrap();
    assert_eq!(authority::authority().len(), before);
    for directory in ["caps", "runtime", "procdata"] {
        assert!(!fixture.root.join(directory).exists());
    }
    let public = crate::clawd::app_sessions::task_host::tests::runtime()
        .block_on(sessions::register(
            json!({"app_id":APP,"kind":"mcp"}),
            &host.client,
        ))
        .unwrap_err();
    assert!(public
        .message
        .contains("App processes cannot manage App sessions"));
    let mut stale = host.client.clone();
    stale.start_time_ticks = stale.start_time_ticks.map(|ticks| ticks + 1);
    assert!(prepare_session_for_task_host(&stale, &parent, &specification(&app)).is_err());
    stale = host.client.clone();
    stale.uid = Some(0);
    assert!(prepare_session_for_task_host(&stale, &parent, &specification(&app)).is_err());
    let mut app_parent = parent.clone();
    app_parent.app_id = Some(APP.to_string());
    assert!(
        prepare_session_for_task_host(&host.client, &app_parent, &specification(&app)).is_err()
    );
}

#[test]
fn session_preparation_requires_the_exact_package_and_signed_session_entry() {
    let mut fixture = Fixture::new();
    let app = fixture.signed_app_with_entries(MANIFEST, &["main.py"]);
    let host = HostProcess::spawn(true);
    let parent = parent(parent_caps());
    let error =
        prepare_session_for_task_host(&host.client, &parent, &specification(&app)).unwrap_err();
    assert!(error.message.contains("signed entrypoint"), "{error}");
    let wrong = TaskHostSession {
        app_id: APP,
        package_digest: "sha256:other",
    };
    assert_eq!(
        prepare_session_for_task_host(&host.client, &parent, &wrong)
            .unwrap_err()
            .kind,
        BrokerErrorKind::Unauthorized,
    );
    let wrong_app = TaskHostSession {
        app_id: "other",
        ..specification(&app)
    };
    assert!(prepare_session_for_task_host(&host.client, &parent, &wrong_app).is_err());
}

#[test]
fn private_session_registration_never_accepts_other_launch_surfaces() {
    let mut fixture = Fixture::new();
    let app = fixture.signed_app_with_entries(MANIFEST, &["server.py"]);
    let host = HostProcess::spawn(true);
    let parent = parent(parent_caps());
    let runtime = crate::clawd::app_sessions::task_host::tests::runtime();
    let before = authority::authority().len();
    for request in [
        json!({"app_id":APP,"kind":"operation","operation":"run"}),
        json!({"app_id":APP,"kind":"gui","operation":"--gui"}),
        json!({"app_id":APP,"kind":"native"}),
        json!({"app_id":APP,"kind":"mcp","command":"/bin/sh"}),
        json!({"app_id":APP,"kind":"mcp","args":["unassigned"]}),
        json!({"app_id":"other","kind":"mcp"}),
    ] {
        assert!(runtime
            .block_on(register_session_for_task_host(
                request,
                &host.client,
                &parent,
                &specification(&app),
            ))
            .is_err());
    }
    assert_eq!(authority::authority().len(), before);
    assert!(!fixture.root.join("procdata").exists());
}

#[test]
fn an_operation_only_package_does_not_become_an_app_owned_session() {
    let mut fixture = Fixture::new();
    let app = fixture.signed_app_with_manifest(
        &json!({
            "id":APP,"version":"0.1.0","name":"No session",
            "operations":{"read":{"label":"Read","args":[],"needs":[]}},
        })
        .to_string(),
    );
    let host = HostProcess::spawn(true);
    let error =
        prepare_session_for_task_host(&host.client, &parent(parent_caps()), &specification(&app))
            .unwrap_err();
    assert!(error.message.contains("no stateful session"));
}

#[test]
fn session_calls_share_default_path_resolution_and_refuse_target_swaps() {
    let mut fixture = Fixture::new();
    let app = fixture.signed_app_with_entries(MANIFEST, &["server.py"]);
    let host = HostProcess::spawn(true);
    let mut parent = parent(parent_caps());
    parent.workdir = Some("/home/stateful".to_string());
    let args = input("a");
    let call = TaskHostSessionCall {
        tool: "session.readwrite",
        args: &args,
    };
    let canonical =
        prepare_session_call_for_task_host(&host.client, &parent, &specification(&app), &call)
            .unwrap();
    assert_eq!(canonical["path"], "/home/stateful/a");
    assert_eq!(canonical["format"], "plain");
    let launcher = authenticate_task_host(&host.client, &parent).unwrap();
    let delegation = task_host_delegation(
        &launcher,
        host.client.uid.unwrap(),
        &host.client.require_home_dir().unwrap(),
        &parent,
        &Value::Null,
    )
    .unwrap();
    validate_call(
        &app,
        &call,
        &json!({"tool":call.tool,"args":canonical}),
        &delegation,
    )
    .unwrap();
    for offered in [
        json!({"tool":call.tool,"args":{"path":"/home/stateful/b"}}),
        json!({"tool":"session.default","args":canonical}),
        json!({"tool":call.tool,"args":canonical,"owner_uid":0}),
        json!({"tool":call.tool,"args":{"path":"/home/stateful/a","unknown":"x"}}),
    ] {
        assert!(validate_call(&app, &call, &offered, &delegation).is_err());
    }
    let defaults = BTreeMap::new();
    let default_call = TaskHostSessionCall {
        tool: "session.default",
        args: &defaults,
    };
    let canonical = prepare_session_call_for_task_host(
        &host.client,
        &parent,
        &specification(&app),
        &default_call,
    )
    .unwrap();
    assert_eq!(canonical["enabled"], false);
    assert_eq!(
        canonical["path"],
        host.client
            .require_home_dir()
            .unwrap()
            .join("state.txt")
            .to_str()
            .unwrap()
    );
    assert!(!fixture.root.join("caps").exists());
}

#[test]
fn developer_trust_cannot_open_a_stateful_session() {
    use crate::provenance::{sign, trust::TrustRootSpec, PackageKind, TrustStore, TrustTier};
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = Fixture::new();
    let app = fixture.signed_app_with_entries(MANIFEST, &["server.py"]);
    let digest = app.require_verified().unwrap().content_digest().to_string();
    std::fs::remove_file(app.dir.join(".provenance.json")).unwrap();
    let body = sign::build_body(
        &app.dir,
        &sign::SignRequest {
            kind: PackageKind::App,
            id: APP.into(),
            version: "0.1.0".into(),
            manifest_schema: "test".into(),
            manifest_path: "app.json".into(),
            entrypoints: vec!["server.py".into()],
            resources: vec![],
        },
    )
    .unwrap();
    assert_eq!(body.content_digest, digest);
    let root = fixture.root.join("developer");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("grants.json");
    std::fs::write(
        &path,
        json!({
            "schema":crate::provenance::trust::DEV_TRUST_SCHEMA_V1,
            "grants":[{"kind":"app","id":APP,"path":app.dir,
                       "content_digest":digest,"granted_at":"2026-01-01T00:00:00Z"}],
        })
        .to_string(),
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let uid = unsafe { libc::geteuid() };
    let roots = vec![TrustRootSpec {
        path: root,
        tier: TrustTier::Developer,
        allowed_uids: vec![uid],
        domain: crate::provenance::state::TrustDomain::Owner(uid),
    }];
    crate::test_env::record_trust_state(&roots);
    crate::provenance::set_trust_store_for_roots(TrustStore::load_roots(&roots), roots);
    let host = HostProcess::spawn(true);
    let error = prepare_session_for_task_host(
        &host.client,
        &parent(parent_caps()),
        &TaskHostSession {
            app_id: APP,
            package_digest: &digest,
        },
    )
    .unwrap_err();
    assert!(
        error.message.contains("does not allow a stdio session"),
        "{error}"
    );
}

struct ProcessPair {
    host: Child,
    client: ClientIdentity,
    child: ProcessIdentity,
}

impl ProcessPair {
    fn new() -> Self {
        let root = unsafe { libc::geteuid() } == 0;
        let uid = if root {
            65534
        } else {
            unsafe { libc::geteuid() }
        };
        let gid = if root {
            65534
        } else {
            unsafe { libc::getegid() }
        };
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 120 & printf '%s\\n' \"$!\"; wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || {
                if root
                    && (libc::setgroups(0, std::ptr::null()) != 0
                        || libc::setresgid(gid, gid, gid) != 0
                        || libc::setresuid(uid, uid, uid) != 0)
                {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut host = command.spawn().unwrap();
        let mut line = String::new();
        BufReader::new(host.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let child = ProcessIdentity::of_process(uid, line.trim().parse().unwrap()).unwrap();
        let client = ClientIdentity {
            pid: Some(host.id()),
            uid: Some(uid),
            gid: Some(gid),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(host.id()),
        };
        Self {
            host,
            client,
            child,
        }
    }
}

impl Drop for ProcessPair {
    fn drop(&mut self) {
        if self.child.still_matches() {
            unsafe {
                libc::kill(
                    libc::pid_t::try_from(self.child.pid).unwrap(),
                    libc::SIGKILL,
                )
            };
        }
        let _ = self.host.kill();
        let _ = self.host.wait();
    }
}

struct Bound {
    fixture: Fixture,
    processes: ProcessPair,
    app: App,
    parent: SessionInfo,
    id: String,
    handle: String,
    relay: String,
}

struct RestoreFile {
    path: PathBuf,
    bytes: Vec<u8>,
}

impl Drop for RestoreFile {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.path, &self.bytes);
    }
}

fn require_private_root() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert_ne!(
        std::fs::read_link("/proc/self/ns/mnt").unwrap(),
        std::fs::read_link("/proc/1/ns/mnt").unwrap(),
        "use an isolated mount namespace, never the live /run",
    );
    assert!(Path::new("/run/.cos-app-host-test").is_file());
    assert!(std::fs::read_to_string("/proc/self/mountinfo")
        .unwrap()
        .lines()
        .any(|line| {
            line.split_once(" - ").is_some_and(|(left, right)| {
                left.split_whitespace().nth(4) == Some("/run")
                    && right.split_whitespace().next() == Some("tmpfs")
            })
        }));
}

impl Bound {
    async fn open(caps: CapSet) -> Self {
        require_private_root();
        let mut fixture = Fixture::private_root();
        let processes = ProcessPair::new();
        let uid = processes.client.uid.unwrap();
        std::env::set_var("COS_PROC_DATA_DIR", format!("/run/cos/caps/{uid}"));
        std::env::remove_var("COS_PROVENANCE_RUNTIME_DIR");
        let app = fixture.signed_app_with_entries(MANIFEST, &["server.py"]);
        let mut parent = parent(caps);
        parent.workdir = Some("/home/stateful".into());
        let result = register_session_for_task_host(
            json!({"app_id":APP,"kind":"mcp"}),
            &processes.client,
            &parent,
            &specification(&app),
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_value::<CapSet>(result["caps"].clone()).unwrap(),
            base_caps(APP)
        );
        let id = result["session_id"].as_str().unwrap().to_string();
        let handle = result["handle"].as_str().unwrap().to_string();
        crate::provenance::runtime::register(uid, &id, app.require_verified().unwrap());
        let bound = sessions::bind(
            json!({"session_id":id,"handle":handle,"pid":processes.child.pid}),
            &processes.client,
        )
        .await
        .unwrap();
        crate::provenance::runtime::bind_process(uid, &id, processes.child.pid);
        Self {
            fixture,
            processes,
            app,
            parent,
            id,
            handle,
            relay: bound["relay_handle"].as_str().unwrap().to_string(),
        }
    }

    fn row(&self) -> SessionInfo {
        crate::proc::session_info_by_id(&self.id).unwrap()
    }

    fn grant(&self) -> Result<authority::GrantView, authority::AuthorityError> {
        let mut presentation = authority::Presentation::new(
            self.processes.client.uid.unwrap(),
            self.processes.child.pid,
            self.processes.child.start_time_ticks,
            authority::Audience::SystemService,
            "test",
        );
        presentation.session_id = Some(self.id.clone());
        authority::authority().resolve_session(&self.id, &presentation)
    }

    fn launch(&self, handle: &str) -> Result<authority::GrantView, String> {
        sessions::require_launch_grant(
            &self.processes.client,
            handle,
            &self.id,
            self.processes.client.uid.unwrap(),
        )
    }

    async fn update(
        &mut self,
        args: Option<&BTreeMap<String, Value>>,
    ) -> Result<Value, BrokerError> {
        let call = args.map(|args| TaskHostSessionCall {
            tool: "session.readwrite",
            args,
        });
        let params = json!({
            "session_id":self.id, "handle":self.handle,
            "call":call.as_ref().map(|call| json!({"tool":call.tool,"args":call.args})),
        });
        let result = set_session_call_for_task_host(
            params,
            &self.processes.client,
            &self.parent,
            &specification(&self.app),
            call.as_ref(),
        )
        .await?;
        self.handle = result["handle"].as_str().unwrap().to_string();
        self.relay = result["relay_handle"].as_str().unwrap().to_string();
        Ok(result)
    }
}

impl Drop for Bound {
    fn drop(&mut self) {
        if let Ok(launch) = self.launch(&self.handle) {
            authority::authority().revoke(launch.id);
        }
        crate::proc::deregister_session_for_owner(&self.id, self.processes.client.uid.unwrap());
        crate::provenance::runtime::deregister(self.processes.client.uid.unwrap(), &self.id);
    }
}

#[tokio::test]
#[ignore = "requires root, private /run tmpfs and /run/.cos-app-host-test"]
async fn stateful_grants_are_exact_at_rest_during_two_calls_and_after_clear() {
    let mut bound = Bound::open(parent_caps()).await;
    assert_eq!(bound.grant().unwrap().caps, base_caps(APP));
    assert!(bound.row().transient_caps.is_none());
    let original_deadline = Instant::now() + bound.launch(&bound.handle).unwrap().expires_in;
    for path in ["/home/stateful/a", "/home/stateful/b"] {
        let old_handle = bound.handle.clone();
        let old_relay = bound.relay.clone();
        bound.update(Some(&input(path))).await.unwrap();
        assert!(bound.launch(&old_handle).is_err());
        assert!(authority::authority()
            .resolve(
                &old_relay,
                &authority::Presentation::new(
                    bound.processes.client.uid.unwrap(),
                    bound.processes.host.id(),
                    bound.processes.client.start_time_ticks,
                    authority::Audience::AppRelay,
                    "test",
                ),
            )
            .is_err());
        let grant = bound.grant().unwrap();
        assert!(grant.expires_in <= ACTIVE_CALL_WINDOW);
        assert!(grant.expires_in > Duration::from_secs(60));
        assert!(bound.launch(&bound.handle).unwrap().expires_in > ACTIVE_CALL_WINDOW);
        let relay = authority::authority()
            .resolve(
                &bound.relay,
                &authority::Presentation::new(
                    bound.processes.client.uid.unwrap(),
                    bound.processes.host.id(),
                    bound.processes.client.start_time_ticks,
                    authority::Audience::AppRelay,
                    "test",
                ),
            )
            .unwrap();
        assert!(relay.expires_in <= ACTIVE_CALL_WINDOW);
        let caps = grant.caps;
        assert_eq!(caps.len(), 3);
        for verb in [Verb::FS_READ, Verb::FS_WRITE] {
            assert!(caps.covers(&Cap::new(verb, Scope::path(path))));
            assert!(!caps.covers(&Cap::new(verb, Scope::path("/home/stateful/other"))));
        }
        assert!(!caps.covers(&Cap::new(Verb::SYS_PACKAGE, Scope::name("nano"))));
        assert!(
            Instant::now() + bound.launch(&bound.handle).unwrap().expires_in <= original_deadline
        );
        assert_eq!(bound.row().caps.unwrap(), base_caps(APP));
        bound.update(None).await.unwrap();
        assert!(bound.row().transient_caps.is_none());
        assert_eq!(bound.grant().unwrap().caps, base_caps(APP));
        assert!(bound.grant().unwrap().expires_in > ACTIVE_CALL_WINDOW);
    }
}

#[tokio::test]
#[ignore = "requires root, private /run tmpfs and /run/.cos-app-host-test"]
async fn clearing_a_retired_call_uses_control_lifetime_without_restoring_effects() {
    let mut bound = Bound::open(parent_caps()).await;
    bound
        .update(Some(&input("/home/stateful/a")))
        .await
        .unwrap();
    let call = bound.grant().unwrap();
    assert!(call.expires_in <= Duration::from_secs(75));
    let control_deadline = Instant::now() + bound.launch(&bound.handle).unwrap().expires_in;
    // A swept expired grant has the same absent index. Exercise that state
    // without weakening or replacing the authority's real expiry checks.
    authority::authority().revoke_session(&bound.id);
    assert!(bound.grant().is_err());
    assert!(bound.launch(&bound.handle).is_ok());
    bound.update(None).await.unwrap();
    let base = bound.grant().unwrap();
    assert_eq!(base.caps, base_caps(APP));
    assert!(base.expires_in > ACTIVE_CALL_WINDOW);
    assert!(Instant::now() + base.expires_in <= control_deadline);
    assert!(bound.row().transient_caps.is_none());
    bound
        .update(Some(&input("/home/stateful/b")))
        .await
        .unwrap();
    assert!(bound.grant().unwrap().expires_in <= ACTIVE_CALL_WINDOW);
    assert!(!bound
        .grant()
        .unwrap()
        .caps
        .covers(&Cap::new(Verb::FS_WRITE, Scope::path("/home/stateful/a")),));
}

#[tokio::test]
#[ignore = "requires root, private /run tmpfs and /run/.cos-app-host-test"]
async fn stateful_call_approvals_are_exact_owner_scoped_and_all_or_none() {
    let mut bound = Bound::open(base_caps(APP)).await;
    let uid = bound.processes.client.uid.unwrap();
    let args = input("/home/stateful/approved");
    let original_handle = bound.handle.clone();
    let error = bound.update(Some(&args)).await.unwrap_err();
    assert_eq!(error.audit_class, Some("approval_required"));
    assert_eq!(
        error.data.as_ref().unwrap()["approval_requests"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let pending = crate::approvals::list_pending_for_owner(Some(uid));
    assert_eq!(pending.len(), 2);
    assert!(pending
        .iter()
        .all(|request| request.session == bound.parent.session_id));
    for (index, request) in pending.iter().enumerate() {
        if index == 1 {
            bound.update(Some(&args)).await.unwrap_err();
            assert!(bound.launch(&original_handle).is_ok());
            assert_eq!(bound.grant().unwrap().caps, base_caps(APP));
            assert!(bound.row().transient_caps.is_none());
            let cap = Cap::new(
                Verb::parse(&pending[0].verb).unwrap(),
                pending[0].scope.clone(),
            );
            assert!(crate::approvals::has_approved_grant_for_owner(
                &bound.parent.session_id,
                &cap,
                Some(uid),
            )
            .unwrap());
            assert!(!crate::approvals::has_approved_grant_for_owner(
                &bound.parent.session_id,
                &cap,
                Some(uid + 1),
            )
            .unwrap());
        }
        crate::approvals::approve_for_owner(
            &request.id,
            crate::approvals::GrantDuration::Once,
            Some("uid:0".into()),
            None,
            Some(uid),
        )
        .unwrap();
    }
    bound.update(Some(&args)).await.unwrap();
    bound.update(None).await.unwrap();
    let denied_again = bound.update(Some(&args)).await.unwrap_err();
    assert_eq!(denied_again.audit_class, Some("approval_required"));
    assert_eq!(bound.grant().unwrap().caps, base_caps(APP));
}

#[tokio::test]
#[ignore = "requires root, private /run tmpfs and /run/.cos-app-host-test"]
async fn stateful_call_refuses_forged_target_parent_caps_and_other_principals() {
    let mut bound = Bound::open(parent_caps()).await;
    let args = input("/home/stateful/a");
    let call = TaskHostSessionCall {
        tool: "session.readwrite",
        args: &args,
    };
    let params = json!({
        "session_id":bound.id,"handle":bound.handle,
        "call":{"tool":call.tool,"args":{"path":"/home/stateful/b"}},
    });
    assert!(set_session_call_for_task_host(
        params,
        &bound.processes.client,
        &bound.parent,
        &specification(&bound.app),
        Some(&call),
    )
    .await
    .is_err());
    let sibling = HostProcess::spawn(true);
    let params = json!({"session_id":bound.id,"handle":bound.handle,"call":null});
    let mut other_session = params.clone();
    other_session["session_id"] = json!("app-other");
    assert!(set_session_call_for_task_host(
        other_session,
        &bound.processes.client,
        &bound.parent,
        &specification(&bound.app),
        None,
    )
    .await
    .is_err());
    let different_package = TaskHostSession {
        app_id: APP,
        package_digest: "sha256:other",
    };
    assert!(set_session_call_for_task_host(
        params.clone(),
        &bound.processes.client,
        &bound.parent,
        &different_package,
        None,
    )
    .await
    .is_err());
    let mut foreign = bound.processes.client.clone();
    foreign.uid = Some(foreign.uid.unwrap() + 1);
    for client in [&sibling.client, &foreign] {
        assert!(set_session_call_for_task_host(
            params.clone(),
            client,
            &bound.parent,
            &specification(&bound.app),
            None,
        )
        .await
        .is_err());
    }
    let mut other_parent = bound.parent.clone();
    other_parent.session_id = "other-parent".to_string();
    assert!(set_session_call_for_task_host(
        params,
        &bound.processes.client,
        &other_parent,
        &specification(&bound.app),
        None,
    )
    .await
    .is_err());
    assert_eq!(bound.grant().unwrap().caps, base_caps(APP));
    let denied = json!({
        "session_id":bound.id, "handle":bound.handle,
        "parent_caps":CapSet::from_caps([Cap::new(Verb::FS_READ,Scope::path("/**"))]),
        "call":{"tool":call.tool, "args":args},
    });
    let error = set_session_call_for_task_host(
        denied,
        &bound.processes.client,
        &bound.parent,
        &specification(&bound.app),
        Some(&call),
    )
    .await
    .unwrap_err();
    assert_eq!(error.audit_class, Some("approval_required"));
    bound.update(Some(&args)).await.unwrap();
    let overlap = bound
        .update(Some(&input("/home/stateful/b")))
        .await
        .unwrap_err();
    assert!(overlap.message.contains("active call"));
    bound.update(None).await.unwrap();
}

#[tokio::test]
#[ignore = "requires root, private /run tmpfs and /run/.cos-app-host-test"]
async fn partial_rotation_failure_clears_the_row_and_revokes_old_and_new_grants() {
    let mut bound = Bound::open(parent_caps()).await;
    let old_handle = bound.handle.clone();
    let before = authority::authority().len();
    FAIL_ROTATION.with(|flag| flag.set(true));
    let error = bound
        .update(Some(&input("/home/stateful/a")))
        .await
        .unwrap_err();
    assert!(error.message.contains("injected session rotation failure"));
    assert!(bound.launch(&old_handle).is_err());
    assert!(bound.grant().is_err());
    assert!(bound.row().transient_caps.is_none());
    assert!(authority::authority().len() < before);
    assert!(
        bound.processes.child.still_matches(),
        "controller owns process retirement"
    );
}

#[tokio::test]
#[ignore = "requires root, private /run tmpfs and /run/.cos-app-host-test"]
async fn failed_clear_never_restores_previous_call_authority() {
    let mut bound = Bound::open(parent_caps()).await;
    bound
        .update(Some(&input("/home/stateful/a")))
        .await
        .unwrap();
    assert!(bound.row().transient_caps.is_some());
    FAIL_ROTATION.with(|flag| flag.set(true));
    bound.update(None).await.unwrap_err();
    assert!(bound.row().transient_caps.is_none());
    assert!(bound.grant().is_err());
    assert!(bound.launch(&bound.handle).is_err());
}

#[tokio::test]
#[ignore = "requires root, private /run tmpfs and /run/.cos-app-host-test"]
async fn failed_registry_transaction_revokes_existing_call_authority() {
    let mut bound = Bound::open(parent_caps()).await;
    bound
        .update(Some(&input("/home/stateful/a")))
        .await
        .unwrap();
    let id = bound.launch(&bound.handle).unwrap().id;
    let registry = crate::proc::registry_path_for_caps();
    let _restore = RestoreFile {
        bytes: std::fs::read(&registry).unwrap(),
        path: registry.clone(),
    };
    std::fs::write(&registry, b"{").unwrap();
    let result: Result<(), BrokerError> = sessions::transient_transaction(
        bound.processes.client.uid.unwrap(),
        bound.processes.client.require_home_dir().unwrap(),
        &bound.id,
        None,
        sessions::TransientRollback::Clear,
        || panic!("an invalid registry must not reach grant issuance"),
        || revoke_launch(id, &bound.id),
    )
    .await;
    assert!(result.is_err());
    assert!(bound.grant().is_err());
    assert!(bound.launch(&bound.handle).is_err());
}

#[tokio::test]
#[ignore = "requires root, private /run tmpfs and /run/.cos-app-host-test"]
async fn clearing_a_replaced_package_revokes_instead_of_reissuing_authority() {
    let mut bound = Bound::open(parent_caps()).await;
    bound
        .update(Some(&input("/home/stateful/a")))
        .await
        .unwrap();
    std::fs::write(
        bound.app.dir.join("server.py"),
        "# changed after verification\n",
    )
    .unwrap();
    let error = bound.update(None).await.unwrap_err();
    assert!(!error.message.is_empty());
    assert!(bound.grant().is_err());
    assert!(bound.launch(&bound.handle).is_err());
    assert!(bound.row().transient_caps.is_none());
}

#[tokio::test]
#[ignore = "requires root, private /run tmpfs and /run/.cos-app-host-test"]
async fn wrong_runtime_process_is_refused_without_signalling_or_rebinding_it() {
    let mut bound = Bound::open(parent_caps()).await;
    let unrelated = HostProcess::spawn(true);
    let uid = bound.processes.client.uid.unwrap();
    let unrelated_pid = unrelated.client.pid.unwrap();
    crate::provenance::runtime::bind_process(uid, &bound.id, unrelated_pid);
    bound
        .update(Some(&input("/home/stateful/a")))
        .await
        .unwrap_err();
    assert!(bound.grant().is_err());
    assert!(bound.launch(&bound.handle).is_err());
    let record = crate::provenance::runtime::instance_for(uid, &bound.id)
        .unwrap()
        .unwrap();
    assert_eq!(record.process.as_ref().unwrap().pid, unrelated_pid);
    assert!(record.process.unwrap().still_matches());
    assert!(bound.processes.child.still_matches());
}

#[tokio::test]
#[ignore = "requires root, private /run tmpfs and /run/.cos-app-host-test"]
async fn revoked_package_cannot_gain_call_authority_and_can_be_cleared() {
    let mut bound = Bound::open(parent_caps()).await;
    let digest = bound
        .app
        .require_verified()
        .unwrap()
        .content_digest()
        .to_string();
    let root = bound.fixture.root.join("trust");
    let key = root.join("publisher.json");
    let mut entry: Value = serde_json::from_slice(&std::fs::read(&key).unwrap()).unwrap();
    entry["revoked_packages"] = json!([digest]);
    std::fs::write(key, entry.to_string()).unwrap();
    let domain = crate::provenance::state::TrustDomain::Owner(unsafe { libc::geteuid() });
    crate::provenance::state::bump(&bound.fixture.root, domain, &[root]).unwrap();
    crate::provenance::reload_trust();
    assert!(bound
        .update(Some(&input("/home/stateful/a")))
        .await
        .is_err());
    assert!(bound.grant().is_err());
    assert!(bound.launch(&bound.handle).is_err());
    assert!(bound.row().transient_caps.is_none());
}
