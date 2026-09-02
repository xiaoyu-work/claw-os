use super::*;

use crate::caps::Verb;

const FS_MANIFEST: &str = r#"{
  "id": "fs",
  "version": "0.1.0",
  "name": "Files",
  "desktop": {"exec": "--gui"},
  "operations": {
    "read": {
      "label": "Read a file",
      "args": [{"name": "path", "kind": "path", "required": true}],
      "needs": [
        {"verb": "fs.read", "scope": {"kind": "from-arg", "arg": "path"}, "why": "read it"}
      ]
    },
    "scan": {
      "label": "Scan",
      "args": [],
      "needs": [{"verb": "fs.meta", "scope": {"kind": "wild"}, "why": "list"}]
    },
    "install": {
      "label": "Install",
      "args": [{"name": "package", "kind": "name", "required": true}],
      "needs": [
        {"verb": "sys.package", "scope": {"kind": "from-arg", "arg": "package"}, "why": "install"}
      ]
    }
  }
}"#;

/// Mirrors `apps/pkg`: a wildcard `sys.package` need with no arguments.
const PKG_MANIFEST: &str = r#"{
  "id": "pkg",
  "version": "0.1.0",
  "name": "Packages",
  "operations": {
    "need": {
      "label": "Ensure installed",
      "args": [],
      "needs": [{"verb": "sys.package", "scope": {"kind": "wild"}, "why": "install"}]
    }
  }
}"#;

/// Mirrors `apps/config-editor`: an argument-bound `sys.config` need.
const CONFIG_EDITOR_MANIFEST: &str = r#"{
  "id": "config-editor",
  "version": "0.1.0",
  "name": "Config Editor",
  "operations": {
    "apply": {
      "label": "Apply configuration",
      "args": [
        {"name": "target", "kind": "path", "required": true},
        {"name": "source", "kind": "path", "required": true}
      ],
      "needs": [
        {"verb": "sys.config", "scope": {"kind": "from-arg", "arg": "target"}, "why": "edit"},
        {"verb": "fs.read", "scope": {"kind": "from-arg", "arg": "source"}, "why": "read"}
      ]
    }
  }
}"#;

/// Mirrors `apps/user-manager`: a manifest-fixed `sys.identity` need.
const USER_MANAGER_MANIFEST: &str = r#"{
  "id": "user-manager",
  "version": "0.1.0",
  "name": "Users",
  "operations": {
    "create-user": {
      "label": "Create a user",
      "args": [{"name": "user", "kind": "name", "required": true}],
      "needs": [
        {
          "verb": "sys.identity",
          "scope": {"kind": "fixed", "scope": {"kind": "name", "value": "accounts"}},
          "why": "manage accounts"
        }
      ]
    }
  }
}"#;

const EMAIL_MCP_MANIFEST: &str = r#"{
  "schema_version": 2,
  "id": "email",
  "version": "1.0.0",
  "name": "Email",
  "mcp": {
    "tools": [{
      "name": "email.search",
      "summary": "Search mail."
    }]
  }
}"#;

fn app_from(manifest: &str) -> App {
    let manifest = Manifest::from_json(manifest).expect("test manifest");
    let dir = std::path::PathBuf::from("/usr/lib/cos/apps").join(&manifest.id);
    App {
        manifest,
        dir,
        provenance: Err("test fixture is not an installed package".to_string()),
    }
}

fn test_app() -> App {
    app_from(FS_MANIFEST)
}

fn args(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|token| (*token).to_string()).collect()
}

fn home() -> std::path::PathBuf {
    std::path::PathBuf::from("/home/u")
}

fn unregistered_ceiling() -> CapSet {
    super::super::system_caps::local_launcher_ceiling(&home())
}

fn home_reader_ceiling() -> CapSet {
    CapSet::from_caps([
        Cap::new(Verb::FS_READ, Scope::path("/home/u/**")),
        Cap::new(Verb::FS_META, Scope::path("/home/u/**")),
        Cap::new(Verb::AGENT_INVOKE, Scope::name("fs")),
    ])
}

fn delegation(ceiling: CapSet) -> Delegation {
    Delegation {
        uid: 4242,
        grant_session: "cli-test".to_string(),
        requester: "uid:4242 pid:1 start:1".to_string(),
        ceiling,
        paths: crate::caps::args::PathContext {
            home: home(),
            cwd: Some(home()),
        },
    }
}

#[test]
fn daemon_plan_skips_inactive_calendar_provider_needs() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("apps/calendar/app.json");
    let manifest = Manifest::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
    let operation = &manifest.operations["today"];
    let values = BTreeMap::from([("provider".to_string(), serde_json::json!("local"))]);
    let resolved = manifest.resolve_needs("today", &values).unwrap();

    let local = Cap::new(Verb::DATA_DB_READ, Scope::name("calendar"));
    let google = Cap::new(
        Verb::SECRET_READ,
        Scope::name("default/GOOGLE_ACCESS_TOKEN"),
    );
    let plan = derive_plan(
        &operation.needs,
        &resolved,
        &delegation(CapSet::from_iter([local.clone(), google.clone()])),
        &publisher_ceiling(),
        "calendar",
    )
    .unwrap();

    assert!(plan.caps.covers(&local));
    assert!(!plan.caps.covers(&google));
    assert!(plan.missing.is_empty());
}

/// Derive and settle a plan the way egister does, so tests exercise
/// the real authorization path.
fn operation_caps(
    app: &App,
    operation: &str,
    args: &[String],
    delegation: &Delegation,
) -> Result<CapSet, BrokerError> {
    let plan = operation_plan(app, operation, args, delegation, &publisher_ceiling())?;
    authorize_plan(delegation, plan, &publisher_ceiling(), &app.manifest.id)
}

fn gui_caps(app: &App, exec: &str, delegation: &Delegation) -> Result<CapSet, BrokerError> {
    let plan = gui_plan(app, exec, delegation, &publisher_ceiling())?;
    authorize_plan(delegation, plan, &publisher_ceiling(), &app.manifest.id)
}

fn with_invoke_cap(
    caps: CapSet,
    app_id: &str,
    delegation: &Delegation,
) -> Result<CapSet, BrokerError> {
    let mut plan = LaunchPlan::default();
    plan.inherit(caps.iter().cloned());
    plan.require(
        Cap::new(Verb::AGENT_INVOKE, Scope::name(app_id)),
        delegation,
    );
    authorize_plan(delegation, plan, &publisher_ceiling(), app_id)
}

/// The ceiling a normally signed package runs under: unrestricted, so
/// these tests exercise the delegation rules rather than the tier. The
/// developer tier has its own dedicated coverage below.
fn publisher_ceiling() -> Ceiling {
    Ceiling::for_tier(crate::provenance::TrustTier::User)
}

#[test]
fn mcp_first_launch_plan_requires_exact_tool_scope() {
    let app = app_from(EMAIL_MCP_MANIFEST);
    let params = serde_json::json!({"kind": "mcp", "tool": "email.search"});
    let (command, invoke) = mcp_launch_command_and_cap(&app, &params).unwrap();
    assert_eq!(command, "cos app email mcp email.search");
    assert_eq!(invoke.scope, Scope::name("email/email.search"));

    let exact = delegation(CapSet::from_iter([invoke.clone()]));
    let mut plan = LaunchPlan::default();
    plan.require(invoke.clone(), &exact);
    assert!(plan.missing.is_empty());
    let target_caps = target_session_caps(plan.caps, true, &invoke);
    assert!(
        !target_caps.covers(&invoke),
        "caller invoke authority must not enter the target App session"
    );

    let coarse = delegation(CapSet::from_iter([Cap::new(
        Verb::AGENT_INVOKE,
        Scope::name("email"),
    )]));
    let mut plan = LaunchPlan::default();
    plan.require(invoke, &coarse);
    assert_eq!(plan.missing.len(), 1);
}

#[test]
fn persistent_gateway_grant_is_exact_and_one_use() {
    let pid = std::process::id();
    let start_time = crate::proc::read_start_time_ticks_pub(pid);
    let owner_uid = 4242;
    let expires_at_ms = crate::agentd::grant::now_ms() + 60_000;
    let binding = crate::extension_host::protocol::ExtensionBinding {
        protocol: crate::extension_host::protocol::PROTOCOL_VERSION,
        mode: crate::extension_host::protocol::ExtensionHostMode::PersistentOwner,
        task_id: "owner-host-4242".to_string(),
        session_id: None,
        owner_uid,
        controller_uid: 0,
        extension_uid: 61_000,
        owner_gid: 1000,
        capability_generation: "0000000000000000".to_string(),
        approved_paths: vec![crate::extension_host::protocol::ApprovedPath {
            path: "/usr/lib/cos/apps".to_string(),
            device: 1,
            inode: 1,
            owner_uid: 0,
            mode: 0o755,
        }],
        worker_pid: pid,
        worker_start_time_ticks: start_time,
        host_pid: pid,
        host_start_time_ticks: start_time,
        lease_nonce: "0123456789abcdef0123456789abcdef".to_string(),
        expires_at_ms,
        control_socket: "/run/cos/test/control.sock".to_string(),
        broker_socket: "/run/cos/test/broker.sock".to_string(),
    };
    let context = crate::agent::tools::app_gateway::McpCallContext::for_authenticated_system_agent(
        owner_uid,
        "agent-session",
        "agent-task",
        Duration::from_secs(30),
        expires_at_ms,
    )
    .unwrap();
    let capability_generation = "0123456789abcdef";
    let package_digest = "a".repeat(64);
    let arguments = serde_json::json!({"query": "Acme"});
    let target = Cap::new(Verb::DATA_DB_READ, Scope::name("email"));
    let handle = issue_gateway_dispatch_grant(
        &binding,
        "email",
        "email.search",
        &arguments,
        &context,
        capability_generation,
        &package_digest,
        CapSet::from_iter([target.clone()]),
    )
    .unwrap();
    let client = crate::clawd::client_identity::ClientIdentity::from_verified_delegation(
        pid,
        owner_uid,
        unsafe { libc::geteuid() as u32 },
        unsafe { libc::getegid() as u32 },
        start_time.unwrap(),
    );
    assert!(matches!(
        crate::clawd::authority::authority().resolve(
            &handle,
            &crate::clawd::authority::Presentation {
                uid: owner_uid,
                pid,
                start_time_ticks: start_time,
                audience: crate::clawd::authority::Audience::AppLaunch,
                route: "app_session",
                session_id: None,
            },
        ),
        Err(crate::clawd::authority::AuthorityError::Audience { .. })
    ));
    let call = serde_json::json!({
        "tool": "email.search",
        "args": arguments,
        "call_id": context.call_id,
        "session_id": context.session_id,
        "task_id": context.task_id,
        "deadline_unix_ms": context.deadline_unix_ms,
        "capability_generation": capability_generation,
        "package_digest": package_digest,
    });
    let mut substituted = call.clone();
    substituted["tool"] = serde_json::json!("email.send");
    assert!(
        consume_gateway_dispatch_grant(&client, &handle, "email", &substituted).is_err(),
        "a substituted target must not spend the exact grant"
    );
    let mut substituted = call.clone();
    substituted["package_digest"] = serde_json::json!("b".repeat(64));
    assert!(
        consume_gateway_dispatch_grant(&client, &handle, "email", &substituted).is_err(),
        "a substituted package must not spend the exact grant"
    );
    let (caps, _) = consume_gateway_dispatch_grant(&client, &handle, "email", &call).unwrap();
    assert!(caps.covers(&target));
    assert!(consume_gateway_dispatch_grant(&client, &handle, "email", &call).is_err());
}

#[test]
fn app_session_registration_rejects_a_substituted_package_identity() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let app_dir = root.path().join("fs");
    std::fs::create_dir_all(&app_dir).unwrap();
    std::fs::write(app_dir.join("app.json"), FS_MANIFEST).unwrap();
    std::fs::write(app_dir.join("main.py"), "print('ok')\n").unwrap();
    crate::test_env::sign_test_package(&app_dir, crate::provenance::PackageKind::App, "fs");
    let app = crate::apps::find_verified(root.path(), "fs").unwrap();
    let expected = crate::provenance::runtime::PackageRef::of(app.require_verified().unwrap());
    assert!(require_expected_package(&app, &expected).is_ok());

    let mut substituted = expected;
    substituted.content_digest = format!("sha256:{}", "0".repeat(64));
    let error = require_expected_package(&app, &substituted).unwrap_err();
    assert!(error.message.contains("package changed"), "{error:?}");
}

#[test]
fn mcp_session_registration_reverifies_its_exact_package_and_command() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let package_dir = root.path().join("org.session-test");
    std::fs::create_dir_all(&package_dir).unwrap();
    std::fs::write(
        package_dir.join("agent-api.json"),
        r#"{"id":"org.session-test","name":"session-test","transport":"mcp+stdio","command":"true"}"#,
    )
    .unwrap();
    crate::test_env::sign_test_package(
        &package_dir,
        crate::provenance::PackageKind::Mcp,
        "org.session-test",
    );
    let package = crate::provenance::verify::verify_package_cached(
        &package_dir,
        &crate::provenance::VerifyOptions::new(crate::provenance::PackageKind::Mcp)
            .expect_id("org.session-test"),
        &crate::provenance::trust_store(),
    )
    .unwrap();
    let package_ref = crate::provenance::runtime::PackageRef::of(&package);
    let params = serde_json::json!({
        "package": {
            "dir": package.dir(),
            "package": package_ref,
        }
    });
    let owner = crate::provenance::fsec::effective_uid();
    assert!(verified_mcp_package(&params, "true", owner)
        .unwrap()
        .is_some());
    assert!(verified_mcp_package(&params, "false", owner).is_err());

    let mut substituted = params;
    substituted["package"]["package"]["content_digest"] =
        serde_json::json!(format!("sha256:{}", "0".repeat(64)));
    assert!(verified_mcp_package(&substituted, "true", owner).is_err());
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn repeated_bind_cannot_destroy_the_first_runtime_binding() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let app_dir = root.path().join("fs");
    std::fs::create_dir_all(&app_dir).unwrap();
    std::fs::write(app_dir.join("app.json"), FS_MANIFEST).unwrap();
    std::fs::write(app_dir.join("main.py"), "print('ok')\n").unwrap();
    crate::test_env::sign_test_package(&app_dir, crate::provenance::PackageKind::App, "fs");
    let app = crate::apps::find_verified(root.path(), "fs").unwrap();
    let package = crate::provenance::runtime::PackageRef::of(app.require_verified().unwrap());
    let ceiling = app.require_verified().unwrap().ceiling();
    let proc_dir = tempfile::tempdir().unwrap();
    let runtime_dir = tempfile::tempdir().unwrap();
    let _apps = crate::test_env::TestEnvVarGuard::set("COS_APPS_DIR", root.path());
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", proc_dir.path());
    let _runtime =
        crate::test_env::TestEnvVarGuard::set("COS_PROVENANCE_RUNTIME_DIR", runtime_dir.path());
    let owner = this_uid();
    let session_id = format!("app-bind-{}", uuid::Uuid::new_v4().simple());
    let launcher = test_authority();
    let caps = home_reader_ceiling();
    let handle = issue_launch_grant(
        &session_id,
        Some("fs"),
        Some(&package),
        owner,
        &launcher,
        &caps,
        Some(&ceiling),
    )
    .unwrap();
    let mut row = e2e_row(&session_id, 0, None);
    row.pid = 0;
    row.pending_bind = true;
    row.start_time_ticks = None;
    row.caps = Some(caps);
    crate::proc::register_session(row).unwrap();
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let params = serde_json::json!({
        "session_id": session_id,
        "handle": handle,
        "pid": child.id(),
    });
    let client = test_client();
    bind(params.clone(), &client).await.unwrap();
    let first = crate::provenance::runtime::instance_for(owner, &session_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        first.process.as_ref().map(|process| process.pid),
        Some(child.id())
    );

    assert!(bind(params, &client).await.is_err());
    let after = crate::provenance::runtime::instance_for(owner, &session_id)
        .unwrap()
        .unwrap();
    assert_eq!(after.process, first.process);

    deregister(
        serde_json::json!({"session_id": session_id, "handle": handle}),
        &client,
    )
    .await
    .unwrap();
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn gateway_authority_rechecks_live_session_narrowing_between_approval_polls() {
    let harness = transient_harness();
    let _proc = crate::test_env::TestEnvVarGuard::set(
        "COS_PROC_DATA_DIR",
        harness._data.path().join("proc"),
    );
    let (worker_pid, worker_start_time_ticks) = this_process();
    let session_id = "agent-gateway-live-session";
    let caps = e2e_app_caps();
    let mut row = e2e_row(session_id, worker_pid, None);
    row.group = Some("agent".to_string());
    row.app_id = None;
    row.caps = Some(caps.clone());
    e2e_install_row(row);
    let authority = AgentGatewayAuthority {
        owner_uid: E2E_UID,
        owner_home: std::path::PathBuf::from("/root"),
        task_id: "task-live-session".to_string(),
        session_id: session_id.to_string(),
        worker_pid,
        worker_start_time_ticks,
        lease_deadline_ms: crate::agentd::grant::now_ms() + 60_000,
        approval_nonce: "0123456789abcdef0123456789abcdef".to_string(),
        approval_expires_at: chrono::Utc::now().timestamp() as u64 + 60,
        consent_context: crate::caps::ConsentContext::Attended,
        capability_generation: crate::agent::tools::exposure::capability_generation(&caps),
        caps,
    };
    e2e_runtime()
        .block_on(verify_agent_gateway_authority(&authority))
        .expect("unchanged live authority");

    let narrowed = CapSet::from_caps([Cap::new(Verb::AGENT_INVOKE, Scope::name("fs"))]);
    let mut row = e2e_row(session_id, worker_pid, None);
    row.group = Some("agent".to_string());
    row.app_id = None;
    row.caps = Some(narrowed);
    e2e_install_row(row);
    let error = e2e_runtime()
        .block_on(verify_agent_gateway_authority(&authority))
        .expect_err("narrowed session must invalidate an approval wait");
    assert!(error.message.contains("capabilities changed"), "{error:?}");
}

#[test]
fn mcp_first_transient_plan_rechecks_and_removes_caller_invoke_cap() {
    let _lock = crate::caps::test_env_lock::env_lock();
    let apps = tempfile::tempdir().unwrap();
    let app_dir = apps.path().join("email");
    std::fs::create_dir_all(&app_dir).unwrap();
    std::fs::write(app_dir.join("app.json"), EMAIL_MCP_MANIFEST).unwrap();
    let _apps = crate::test_env::TestEnvVarGuard::set("COS_APPS_DIR", apps.path());
    let invoke = Cap::new(Verb::AGENT_INVOKE, Scope::name("email/email.search"));
    let delegation = delegation(CapSet::from_iter([invoke.clone()]));
    let app = app_from(EMAIL_MCP_MANIFEST);
    let ceiling = Ceiling::for_package(crate::provenance::TrustTier::Vendor, "email");
    let deadline = crate::agentd::grant::now_ms() + 1000;
    let (plan, caller_authority) = session_tool_plan(
        &app,
        &serde_json::json!({
            "tool": "email.search",
            "args": {},
            "deadline_unix_ms": deadline
        }),
        &delegation,
        &ceiling,
    )
    .unwrap();
    let caller_authority = caller_authority.expect("MCP call authority");
    assert_eq!(caller_authority.invoke, invoke);
    assert_eq!(caller_authority.deadline_unix_ms, deadline);
    assert!(plan.missing.is_empty());
    assert!(plan.caps.covers(&invoke));
    let target = target_session_caps(plan.caps, true, &invoke);
    assert!(!target.covers(&invoke));
    assert!(session_tool_plan(
        &app,
        &serde_json::json!({"tool": "email.search", "args": {}}),
        &delegation,
        &ceiling,
    )
    .is_err());
    assert!(session_tool_plan(
        &app,
        &serde_json::json!({
            "tool": "email.search",
            "args": {},
            "deadline_unix_ms": 1
        }),
        &delegation,
        &ceiling,
    )
    .is_err());
}

#[test]
fn gateway_target_grant_can_exceed_invoke_only_launch_authority() {
    let _lock = crate::caps::test_env_lock::env_lock();
    authority::authority().clear_for_test();
    let uid = unsafe { libc::geteuid() as u32 };
    let pid = std::process::id();
    let session_id = format!("gateway-target-{}", uuid::Uuid::new_v4().simple());
    let target = CapSet::from_caps([Cap::new(Verb::FS_READ, Scope::path("/srv/customer/**"))]);
    issue_gateway_target_grant(
        &session_id,
        "email",
        uid,
        pid,
        &target,
        crate::agentd::grant::now_ms() + 60_000,
        None,
    )
    .unwrap();
    let view = authority::authority()
        .resolve_session(
            &session_id,
            &authority::Presentation {
                uid,
                pid,
                start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
                audience: authority::Audience::SystemService,
                route: "test",
                session_id: Some(session_id.clone()),
            },
        )
        .unwrap();
    assert_eq!(view.issuer, authority::Issuer::AppGateway);
    assert!(view
        .caps
        .covers(&Cap::new(Verb::FS_READ, Scope::path("/srv/customer/file"))));
    authority::authority().clear_for_test();
}

/// Launcher authority for a synthetic peer process.
fn authority_for(
    pid: u32,
    start_time_ticks: Option<u64>,
    parent: Option<&str>,
) -> LauncherAuthority {
    LauncherAuthority {
        pid,
        start_time_ticks,
        parent: parent.map(ToOwned::to_owned),
        caps: unregistered_ceiling(),
        tier: None,
        scope: None,
        priority: None,
        role: None,
    }
}

/// Delegation for an unregistered launcher, exactly as egister
/// builds it.
fn launcher_delegation(pid: u32, ticks: u64) -> Delegation {
    Delegation::new(
        &authority_for(pid, Some(ticks), None),
        1000,
        &home(),
        &serde_json::json!({}),
    )
    .expect("delegation")
}

fn session_row(session_id: &str, pid: u32, app_id: Option<&str>, caps: CapSet) -> SessionInfo {
    SessionInfo {
        session_id: session_id.to_string(),
        pid,
        command: vec!["test".to_string()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("cron".to_string()),
        parent: None,
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: Some(Role::AgentHost.credential_tier()),
        scope: None,
        priority: None,
        caps: Some(caps),
        transient_caps: None,
        role: Some(Role::AgentHost.name().to_string()),
        app_id: app_id.map(ToOwned::to_owned),
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
        client: crate::session::SessionClient::default(),
    }
}

fn this_process() -> (u32, Option<u64>) {
    let pid = std::process::id();
    (pid, crate::proc::read_start_time_ticks_pub(pid))
}

/// Redirect the approvals store into a throwaway directory for tests
/// that exercise a denial or grant, so they never touch real state.
/// Holds the shared env lock for the duration.
struct ApprovalSandbox {
    _lock: std::sync::MutexGuard<'static, ()>,
    prev: Option<std::ffi::OsString>,
    _tmp: tempfile::TempDir,
}

impl Drop for ApprovalSandbox {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(value) => std::env::set_var("COS_CAPS_DATA_DIR", value),
            None => std::env::remove_var("COS_CAPS_DATA_DIR"),
        }
    }
}

fn approval_sandbox() -> ApprovalSandbox {
    let lock = crate::caps::test_env_lock::env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev = std::env::var_os("COS_CAPS_DATA_DIR");
    std::env::set_var("COS_CAPS_DATA_DIR", tmp.path());
    ApprovalSandbox {
        _lock: lock,
        prev,
        _tmp: tmp,
    }
}

/// Approve whatever the launcher's last denial filed.
fn approve_only_pending(uid: u32, duration: crate::approvals::GrantDuration) -> String {
    let pending = crate::approvals::list_pending_for_owner(Some(uid));
    assert_eq!(pending.len(), 1, "expected exactly one pending request");
    crate::approvals::approve_for_owner(
        &pending[0].id,
        duration,
        Some("uid:0".to_string()),
        None,
        Some(uid),
    )
    .expect("approve");
    pending[0].id.clone()
}

/// Approval request ids a denial reported, as the launcher sees them.
fn approval_requests(error: &BrokerError) -> Vec<String> {
    let data = error
        .data
        .as_ref()
        .expect("a denial must report its requests");
    assert_eq!(data["status"], Value::String("approval_required".into()));
    data["approval_requests"]
        .as_array()
        .expect("approval_requests")
        .iter()
        .map(|id| id.as_str().expect("id").to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Unregistered-launcher policy
// ---------------------------------------------------------------------------

#[test]
fn unregistered_launcher_falls_back_to_daemon_policy_not_a_claimed_authority() {
    let (pid, ticks) = this_process();
    let authority = launcher_authority(&[], pid, ticks, &home()).expect("policy ceiling");
    assert!(authority.parent.is_none());
    assert_eq!(authority.role.as_deref(), Some(Role::Worker.name()));
    assert!(authority
        .caps
        .covers(&Cap::new(Verb::AGENT_INVOKE, Scope::name("fs"))));
}

#[test]
fn unregistered_ceiling_excludes_machine_mutating_authority() {
    let ceiling = unregistered_ceiling();
    for verb in [
        Verb::SYS_CONFIG,
        Verb::SYS_PACKAGE,
        Verb::SYS_IDENTITY,
        Verb::SYS_SERVICE,
        Verb::SYS_STORAGE,
        Verb::SYS_MOUNT,
        Verb::SYS_SNAPSHOT,
        Verb::SYS_SECURITY,
        Verb::SYS_POWER,
        Verb::NET_FIREWALL,
        Verb::NET_MANAGE,
        Verb::DATA_BACKUP,
        Verb::SECRET_READ,
        Verb::SECRET_WRITE,
        Verb::SECRET_GRANT,
    ] {
        assert!(
            !ceiling.verbs().contains(&verb),
            "{} must never be delegated to an unregistered launcher",
            verb.as_str()
        );
        assert!(!ceiling.covers(&Cap::new(verb, Scope::Wild)));
        assert!(!ceiling.covers(&Cap::new(verb, Scope::name("**"))));
        assert!(!ceiling.covers(&Cap::new(verb, Scope::path("/**"))));
    }
}

#[test]
fn unregistered_ceiling_has_no_global_filesystem_authority() {
    let ceiling = unregistered_ceiling();
    for verb in [Verb::FS_READ, Verb::FS_META, Verb::FS_WRITE] {
        assert!(
            !ceiling.covers(&Cap::new(verb, Scope::path("/**"))),
            "{} must not be granted globally",
            verb.as_str()
        );
        assert!(!ceiling.covers(&Cap::new(verb, Scope::path("/etc/shadow"))));
        assert!(!ceiling.covers(&Cap::new(verb, Scope::Wild)));
        assert!(
            ceiling.covers(&Cap::new(verb, Scope::path("/home/u/notes.txt"))),
            "{} inside the caller's home stays available",
            verb.as_str()
        );
    }
    assert!(!ceiling.covers(&Cap::new(Verb::FS_DELETE, Scope::path("/home/u/notes.txt"))));
    assert!(!ceiling.covers(&Cap::new(Verb::FS_EXEC, Scope::path("/home/u/x.sh"))));
}

#[test]
fn unregistered_launcher_cannot_reach_privileged_first_party_apps() {
    let _sandbox = approval_sandbox();
    let delegation = delegation(unregistered_ceiling());

    let error = operation_caps(&app_from(PKG_MANIFEST), "need", &[], &delegation)
        .expect_err("wildcard sys.package must not be inheritable");
    assert!(error.message.contains("sys.package"), "unexpected: {error}");

    let error = operation_caps(
        &app_from(CONFIG_EDITOR_MANIFEST),
        "apply",
        &args(&["/etc/cos/agent.toml", "/home/u/evil.toml"]),
        &delegation,
    )
    .expect_err("sys.config must not be delegated without a grant");
    assert!(error.message.contains("sys.config"), "unexpected: {error}");

    let error = operation_caps(
        &app_from(USER_MANAGER_MANIFEST),
        "create-user",
        &args(&["attacker"]),
        &delegation,
    )
    .expect_err("sys.identity must not be delegated without a grant");
    assert!(
        error.message.contains("sys.identity"),
        "unexpected: {error}"
    );

    let error = operation_caps(&test_app(), "install", &args(&["vim"]), &delegation)
        .expect_err("argument-bound sys.package must not be delegated either");
    assert!(error.message.contains("sys.package"), "unexpected: {error}");
}

#[test]
fn denied_privileged_operation_files_one_approval_request() {
    let _sandbox = approval_sandbox();
    let delegation = delegation(unregistered_ceiling());
    let app = app_from(USER_MANAGER_MANIFEST);

    let first = operation_caps(&app, "create-user", &args(&["attacker"]), &delegation)
        .expect_err("denied without an approved grant");
    assert_eq!(approval_requests(&first).len(), 1);
    let pending = crate::approvals::list_pending_for_owner(Some(delegation.uid));
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].verb, Verb::SYS_IDENTITY.as_str());
    assert_eq!(pending[0].session, delegation.grant_session);

    let _ = operation_caps(&app, "create-user", &args(&["attacker"]), &delegation)
        .expect_err("still denied while the request is pending");
    assert_eq!(
        crate::approvals::list_pending_for_owner(Some(delegation.uid)).len(),
        1
    );
}

#[test]
fn approved_grant_authorizes_one_privileged_launch() {
    let _sandbox = approval_sandbox();
    let delegation = delegation(unregistered_ceiling());
    let app = app_from(USER_MANAGER_MANIFEST);

    let id = crate::approvals::submit_owned(
        Verb::SYS_IDENTITY,
        Scope::name("accounts"),
        delegation.grant_session.clone(),
        "test",
        None,
        Some(delegation.uid),
    )
    .expect("submit");
    crate::approvals::approve_for_owner(
        &id,
        crate::approvals::GrantDuration::Once,
        Some("uid:0".to_string()),
        None,
        Some(delegation.uid),
    )
    .expect("approve");

    let caps = operation_caps(&app, "create-user", &args(&["attacker"]), &delegation)
        .expect("an approved grant authorises the launch");
    assert!(caps.covers(&Cap::new(Verb::SYS_IDENTITY, Scope::name("accounts"))));

    let error = operation_caps(&app, "create-user", &args(&["attacker"]), &delegation)
        .expect_err("the grant is single use");
    assert!(
        error.message.contains("sys.identity"),
        "unexpected: {error}"
    );
}

// ---------------------------------------------------------------------------
// Grant identity
// ---------------------------------------------------------------------------

#[test]
fn delegation_never_accepts_a_caller_asserted_parent_session() {
    let authority = authority_for(4242, Some(91_234), None);
    let asserted = serde_json::json!({"parent_session": "cli-victim-session"});
    let delegation = Delegation::new(&authority, 1000, &home(), &asserted).expect("delegation");
    assert_ne!(delegation.grant_session, "cli-victim-session");
    assert_eq!(
        delegation.grant_session,
        unregistered_grant_identity(1000, 4242, Some(91_234)).unwrap(),
        "the grant identity comes from the peer, not the request"
    );
}

#[test]
fn delegation_uses_the_authenticated_parent_when_one_exists() {
    let authority = authority_for(4242, Some(91_234), Some("cron-1"));
    let asserted = serde_json::json!({"parent_session": "cli-victim-session"});
    let delegation = Delegation::new(&authority, 1000, &home(), &asserted).expect("delegation");
    assert_eq!(delegation.grant_session, "cron-1");
}

#[test]
fn delegation_records_authenticated_requester_facts() {
    let authority = authority_for(4242, Some(91_234), None);
    let delegation =
        Delegation::new(&authority, 1000, &home(), &serde_json::json!({})).expect("delegation");
    assert_eq!(delegation.requester, "uid:1000 pid:4242 start:91234");
}

#[test]
fn unregistered_grant_identity_binds_to_the_exact_launcher_process() {
    let launcher = unregistered_grant_identity(1000, 4242, Some(91_234)).unwrap();
    assert_eq!(
        launcher,
        unregistered_grant_identity(1000, 4242, Some(91_234)).unwrap(),
        "the same launcher derives the same identity for filing and consuming"
    );
    for other in [
        unregistered_grant_identity(1000, 4243, Some(91_234)).unwrap(),
        unregistered_grant_identity(1000, 4242, Some(91_999)).unwrap(),
        unregistered_grant_identity(1001, 4242, Some(91_234)).unwrap(),
    ] {
        assert_ne!(
            launcher, other,
            "a different peer must derive a different identity"
        );
    }
}

#[test]
fn unregistered_grant_identity_requires_authenticated_process_facts() {
    assert!(unregistered_grant_identity(1000, 4242, None).is_err());
    let authority = authority_for(4242, None, None);
    assert!(Delegation::new(&authority, 1000, &home(), &serde_json::json!({})).is_err());
}

#[test]
fn siblings_in_one_login_share_no_launch_identity() {
    // Two `cos` processes started from the same terminal differ only in
    // pid/start time; nothing else may make their identities equal.
    let first = unregistered_grant_identity(1000, 4242, Some(91_234)).unwrap();
    let second = unregistered_grant_identity(1000, 4243, Some(91_240)).unwrap();
    assert_ne!(first, second);
}

#[test]
fn another_same_uid_launcher_cannot_consume_an_approved_request() {
    let _sandbox = approval_sandbox();
    let app = app_from(USER_MANAGER_MANIFEST);
    let victim = launcher_delegation(4242, 91_234);
    let attacker = launcher_delegation(4243, 91_555);

    let denial = operation_caps(&app, "create-user", &args(&["alice"]), &victim)
        .expect_err("the victim launch is denied first");
    let pending = crate::approvals::list_pending_for_owner(Some(1000));
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].session, victim.grant_session);
    assert_eq!(
        pending[0].requester.as_deref(),
        Some("uid:1000 pid:4242 start:91234")
    );
    assert_eq!(approval_requests(&denial).len(), 1);
    approve_only_pending(1000, crate::approvals::GrantDuration::Once);

    // The sibling can read the victim's pending request and its session
    // string, but cannot make the daemon derive that identity for its
    // own connection.
    let error = operation_caps(&app, "create-user", &args(&["alice"]), &attacker)
        .expect_err("a sibling process must not consume someone else's approval");
    assert!(
        error.message.contains("sys.identity"),
        "unexpected: {error}"
    );

    let caps = operation_caps(&app, "create-user", &args(&["alice"]), &victim)
        .expect("the launcher the grant was issued to may use it");
    assert!(caps.covers(&Cap::new(Verb::SYS_IDENTITY, Scope::name("accounts"))));
}

#[test]
fn approved_grant_matching_stays_exact() {
    let _sandbox = approval_sandbox();
    let delegation = delegation(unregistered_ceiling());
    let app = app_from(USER_MANAGER_MANIFEST);

    for (verb, scope) in [
        (Verb::SYS_IDENTITY, Scope::name("other")),
        (Verb::SYS_CONFIG, Scope::path("/etc/other")),
    ] {
        let id = crate::approvals::submit_owned(
            verb,
            scope,
            delegation.grant_session.clone(),
            "test",
            None,
            Some(delegation.uid),
        )
        .expect("submit");
        crate::approvals::approve_for_owner(
            &id,
            crate::approvals::GrantDuration::Once,
            Some("uid:0".to_string()),
            None,
            Some(delegation.uid),
        )
        .expect("approve");
    }

    let error = operation_caps(&app, "create-user", &args(&["alice"]), &delegation)
        .expect_err("neither a different scope nor a different verb may authorise the launch");
    assert!(
        error.message.contains("sys.identity"),
        "unexpected: {error}"
    );
}

// ---------------------------------------------------------------------------
// Launch authorization plan
// ---------------------------------------------------------------------------

#[test]
fn a_launch_short_several_capabilities_requests_them_all_at_once() {
    let _sandbox = approval_sandbox();
    let app = app_from(CONFIG_EDITOR_MANIFEST);
    let launcher = launcher_delegation(4242, 91_234);

    // Both the system target and a source outside the caller's home are
    // beyond the unregistered ceiling.
    let denial = operation_caps(
        &app,
        "apply",
        &args(&["/etc/cos/agent.toml", "/etc/other.toml"]),
        &launcher,
    )
    .expect_err("two capabilities are missing");
    let ids = approval_requests(&denial);
    assert_eq!(ids.len(), 2, "every missing capability must be requested");

    let pending = crate::approvals::list_pending_for_owner(Some(1000));
    assert_eq!(pending.len(), 2);
    let mut verbs: Vec<&str> = pending.iter().map(|r| r.verb.as_str()).collect();
    verbs.sort_unstable();
    assert_eq!(verbs, vec!["fs.read", "sys.config"]);

    // Approving only one must not settle or burn anything.
    crate::approvals::approve_for_owner(
        &ids[0],
        crate::approvals::GrantDuration::Once,
        Some("uid:0".to_string()),
        None,
        Some(1000),
    )
    .expect("approve one");
    let denial = operation_caps(
        &app,
        "apply",
        &args(&["/etc/cos/agent.toml", "/etc/other.toml"]),
        &launcher,
    )
    .expect_err("still short one capability");
    assert_eq!(
        approval_requests(&denial),
        vec![ids[1].clone()],
        "an already-approved capability is not queued for a second decision"
    );
    assert_eq!(
        crate::approvals::status_for_owner(&ids[0], Some(1000)),
        crate::approvals::RequestStatus::Approved,
        "the already-approved half must not have been burned"
    );

    crate::approvals::approve_for_owner(
        &ids[1],
        crate::approvals::GrantDuration::Once,
        Some("uid:0".to_string()),
        None,
        Some(1000),
    )
    .expect("approve the rest");
    let caps = operation_caps(
        &app,
        "apply",
        &args(&["/etc/cos/agent.toml", "/etc/other.toml"]),
        &launcher,
    )
    .expect("the complete set settles the launch");
    assert!(caps.covers(&Cap::new(
        Verb::SYS_CONFIG,
        Scope::path("/etc/cos/agent.toml")
    )));
    assert!(caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/etc/other.toml"))));
    for id in &ids {
        assert_eq!(
            crate::approvals::status_for_owner(id, Some(1000)),
            crate::approvals::RequestStatus::Consumed
        );
    }
}

#[test]
fn the_same_launcher_process_can_retry_after_approval() {
    let _sandbox = approval_sandbox();
    let app = app_from(USER_MANAGER_MANIFEST);
    let launcher = launcher_delegation(4242, 91_234);

    let denial = operation_caps(&app, "create-user", &args(&["alice"]), &launcher)
        .expect_err("the first attempt is denied");
    let ids = approval_requests(&denial);
    assert_eq!(ids.len(), 1);
    assert_eq!(
        crate::approvals::status_for_owner(&ids[0], Some(1000)),
        crate::approvals::RequestStatus::Pending
    );

    crate::approvals::approve_for_owner(
        &ids[0],
        crate::approvals::GrantDuration::Once,
        Some("uid:0".to_string()),
        None,
        Some(1000),
    )
    .expect("approve");

    // The same process — same uid, pid and start time — retries and the
    // daemon re-derives the identity the grant is bound to.
    let retry = launcher_delegation(4242, 91_234);
    assert_eq!(retry.grant_session, launcher.grant_session);
    let caps = operation_caps(&app, "create-user", &args(&["alice"]), &retry)
        .expect("the approved launch proceeds");
    assert!(caps.covers(&Cap::new(Verb::SYS_IDENTITY, Scope::name("accounts"))));

    let again = operation_caps(&app, "create-user", &args(&["alice"]), &retry)
        .expect_err("the grant is spent");
    assert_eq!(approval_requests(&again).len(), 1);
}

#[test]
fn an_approved_scope_does_not_settle_a_different_one() {
    let _sandbox = approval_sandbox();
    let app = app_from(CONFIG_EDITOR_MANIFEST);
    let launcher = launcher_delegation(4242, 91_234);

    let denial = operation_caps(
        &app,
        "apply",
        &args(&["/etc/cos/agent.toml", "/home/u/source.toml"]),
        &launcher,
    )
    .expect_err("sys.config is missing");
    let ids = approval_requests(&denial);
    assert_eq!(ids.len(), 1);
    crate::approvals::approve_for_owner(
        &ids[0],
        crate::approvals::GrantDuration::Once,
        Some("uid:0".to_string()),
        None,
        Some(1000),
    )
    .expect("approve");

    // A different target derives a different canonical scope, so the
    // approval does not apply and nothing is consumed.
    let denial = operation_caps(
        &app,
        "apply",
        &args(&["/etc/passwd", "/home/u/source.toml"]),
        &launcher,
    )
    .expect_err("another resource is not covered");
    assert!(
        denial.message.contains("/etc/passwd"),
        "unexpected: {denial}"
    );
    assert_eq!(
        crate::approvals::status_for_owner(&ids[0], Some(1000)),
        crate::approvals::RequestStatus::Approved
    );
}

#[test]
fn reusable_grant_durations_do_not_create_ambient_launch_authority() {
    for duration in [
        crate::approvals::GrantDuration::Session,
        crate::approvals::GrantDuration::Forever,
    ] {
        let _sandbox = approval_sandbox();
        let app = app_from(USER_MANAGER_MANIFEST);
        let launcher = launcher_delegation(4242, 91_234);

        let denial =
            operation_caps(&app, "create-user", &args(&["alice"]), &launcher).expect_err("denied");
        let ids = approval_requests(&denial);
        crate::approvals::approve_for_owner(
            &ids[0],
            duration,
            Some("uid:0".to_string()),
            None,
            Some(1000),
        )
        .expect("approve");

        assert!(
            operation_caps(&app, "create-user", &args(&["alice"]), &launcher).is_ok(),
            "the approval still authorises one launch"
        );
        let error = operation_caps(&app, "create-user", &args(&["alice"]), &launcher)
            .expect_err("a broader duration must not survive the first App launch");
        assert_eq!(approval_requests(&error).len(), 1);
    }
}

// ---------------------------------------------------------------------------
// Launcher authority
// ---------------------------------------------------------------------------

#[test]
fn registered_parent_row_is_the_ceiling() {
    let (pid, ticks) = this_process();
    let rows = vec![session_row("cron-1", pid, None, home_reader_ceiling())];
    let authority = launcher_authority(&rows, pid, ticks, &home()).expect("parent authority");
    assert_eq!(authority.parent.as_deref(), Some("cron-1"));
    assert!(authority
        .caps
        .covers(&Cap::new(Verb::FS_READ, Scope::path("/home/u/a.txt"))));
    assert!(
        !authority
            .caps
            .covers(&Cap::new(Verb::SYS_PACKAGE, Scope::name("vim"))),
        "the parent row, not the daemon default, bounds a bound launcher"
    );
}

#[test]
fn an_app_session_cannot_mint_further_sessions() {
    let (pid, ticks) = this_process();
    let rows = vec![session_row("app-1", pid, Some("fs"), home_reader_ceiling())];
    let error =
        launcher_authority(&rows, pid, ticks, &home()).expect_err("App launchers are rejected");
    assert!(error.contains("app-1"), "unexpected error: {error}");
}

#[test]
fn only_the_exact_broker_registered_extension_host_may_launch_under_nonewprivs() {
    let (pid, ticks) = this_process();
    let mut host = session_row("extension-1", pid, None, home_reader_ceiling());
    host.group = Some(crate::extension_host::protocol::EXTENSION_HOST_GROUP.to_string());
    host.start_time_ticks = ticks;
    assert!(is_trusted_extension_host_launcher(
        &[host.clone()],
        pid,
        ticks
    ));

    host.group = Some("worker".to_string());
    assert!(!is_trusted_extension_host_launcher(
        &[host.clone()],
        pid,
        ticks
    ));

    host.group = Some(crate::extension_host::protocol::EXTENSION_HOST_GROUP.to_string());
    host.app_id = Some("evil".to_string());
    assert!(!is_trusted_extension_host_launcher(&[host], pid, ticks));
}

#[test]
fn stale_registry_row_does_not_authorize_a_recycled_pid() {
    let (pid, ticks) = this_process();
    let mut row = session_row("cron-1", pid, None, home_reader_ceiling());
    row.start_time_ticks = Some(row.start_time_ticks.unwrap_or_default().wrapping_add(4242));
    let authority = launcher_authority(&[row], pid, ticks, &home()).expect("policy ceiling");
    assert!(
        authority.parent.is_none(),
        "a row whose process start time no longer matches must not be adopted"
    );
}

#[test]
fn unresolvable_ancestry_fails_closed() {
    let error = nearest_registered_session(&[], u32::MAX)
        .expect_err("unresolvable ancestry must not silently mean `unregistered`");
    assert!(error.contains("ancestry"), "unexpected error: {error}");

    let error = launcher_authority(&[], u32::MAX, None, &home())
        .expect_err("the authority must fail closed too");
    assert!(error.contains("ancestry"), "unexpected error: {error}");
}

#[test]
fn clean_walk_without_a_registered_session_is_not_an_error() {
    let (pid, _) = this_process();
    assert!(nearest_registered_session(&[], pid)
        .expect("a complete walk is not a failure")
        .is_none());
}

#[test]
fn declared_parent_caps_only_narrow_the_ceiling() {
    let (pid, ticks) = this_process();
    let authority = launcher_authority(
        &[session_row("cron-1", pid, None, home_reader_ceiling())],
        pid,
        ticks,
        &home(),
    )
    .expect("parent authority");

    let widened = serde_json::json!({
        "parent_caps": serde_json::to_value(CapSet::from_caps([
            Cap::new(Verb::SYS_PACKAGE, Scope::Wild),
            Cap::new(Verb::FS_READ, Scope::path("/**")),
        ]))
        .expect("serialize caps")
    });
    let ceiling = attenuated_ceiling(&authority, &widened).expect("attenuated ceiling");
    assert!(
        !ceiling.covers(&Cap::new(Verb::SYS_PACKAGE, Scope::name("vim"))),
        "caller-declared capabilities must never widen the authenticated ceiling"
    );
    assert!(
        !ceiling.covers(&Cap::new(Verb::FS_READ, Scope::path("/etc/shadow"))),
        "caller-declared scopes must never widen the authenticated ceiling"
    );

    let narrowed = serde_json::json!({
        "parent_caps": serde_json::to_value(CapSet::from_caps([Cap::new(
            Verb::FS_META,
            Scope::path("/home/u/**")
        )]))
        .expect("serialize caps")
    });
    let ceiling = attenuated_ceiling(&authority, &narrowed).expect("attenuated ceiling");
    assert!(!ceiling.covers(&Cap::new(Verb::FS_READ, Scope::path("/home/u/a.txt"))));
    assert!(ceiling.covers(&Cap::new(Verb::FS_META, Scope::path("/home/u/a.txt"))));
}

// ---------------------------------------------------------------------------
// Manifest-derived capabilities
// ---------------------------------------------------------------------------

#[test]
fn operation_caps_bind_the_exact_argument_value() {
    let delegation = delegation(home_reader_ceiling());
    let caps = operation_caps(
        &test_app(),
        "read",
        &args(&["/home/u/notes.txt"]),
        &delegation,
    )
    .expect("derived caps");
    assert!(caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/home/u/notes.txt"))));
    assert!(
        !caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/home/u/other.txt"))),
        "the issued capability must be the canonical value, not the whole scope kind"
    );
    assert_eq!(caps.len(), 1);
}

#[test]
fn path_bound_need_cannot_authorize_a_different_path() {
    let _sandbox = approval_sandbox();
    let delegation = delegation(home_reader_ceiling());
    let error = operation_caps(&test_app(), "read", &args(&["/etc/shadow"]), &delegation)
        .expect_err("out-of-ceiling path must be rejected");
    assert!(error.message.contains("fs.read"), "unexpected: {error}");
    assert!(error.message.contains("/etc/shadow"), "unexpected: {error}");
}

#[test]
fn unregistered_launcher_cannot_obtain_capabilities_outside_the_manifest() {
    let delegation = delegation(unregistered_ceiling());
    let caps = operation_caps(
        &test_app(),
        "read",
        &args(&["/home/u/notes.txt"]),
        &delegation,
    )
    .expect("derived caps");
    assert_eq!(
        caps.verbs(),
        vec![Verb::FS_READ],
        "an App session only ever receives what its operation declares"
    );
    assert!(!caps.covers(&Cap::new(Verb::SYS_PACKAGE, Scope::name("vim"))));
}

#[test]
fn wildcard_need_inherits_only_what_the_launcher_holds() {
    let delegation = delegation(home_reader_ceiling());
    let caps = operation_caps(&test_app(), "scan", &[], &delegation).expect("derived caps");
    assert!(caps.covers(&Cap::new(Verb::FS_META, Scope::path("/home/u/a.txt"))));
    assert!(
        !caps.covers(&Cap::new(Verb::FS_META, Scope::path("/etc/passwd"))),
        "a wildcard need must not widen beyond the launcher's own scopes"
    );
}

#[test]
fn wildcard_inheritance_never_exceeds_the_restricted_ceiling() {
    let delegation = delegation(unregistered_ceiling());
    let caps = operation_caps(&test_app(), "scan", &[], &delegation).expect("derived caps");
    for cap in caps.iter() {
        assert!(
            delegation.ceiling.covers(cap),
            "{}:{} is outside the launcher ceiling",
            cap.verb.as_str(),
            cap.scope
        );
    }
    for verb in [Verb::AI_CHAT, Verb::NET_DIAL] {
        inherited_wild_caps(verb, &delegation)
            .expect_err("typed wildcard ceilings are not bounded authority");
    }
}

#[test]
fn wildcard_need_never_inherits_unbounded_resource_authority() {
    let path_wild = delegation(CapSet::from_caps([Cap::new(Verb::FS_META, Scope::Wild)]));
    let error = operation_caps(&test_app(), "scan", &[], &path_wild)
        .expect_err("a wild ceiling must not satisfy a wild need");
    assert!(error.message.contains("unbounded"), "unexpected: {error}");

    let name_wild = delegation(CapSet::from_caps([Cap::new(Verb::AI_CHAT, Scope::Wild)]));
    let error = inherited_wild_caps(Verb::AI_CHAT, &name_wild)
        .expect_err("name-scoped verbs address resources too");
    assert!(error.message.contains("unbounded"), "unexpected: {error}");

    let host_wild = delegation(CapSet::from_caps([Cap::new(Verb::NET_DIAL, Scope::Wild)]));
    let error = inherited_wild_caps(Verb::NET_DIAL, &host_wild)
        .expect_err("host-scoped verbs address resources too");
    assert!(error.message.contains("unbounded"), "unexpected: {error}");

    for scope in [Scope::host("**"), Scope::path("/**"), Scope::name("**")] {
        let typed = delegation(CapSet::from_caps([Cap::new(Verb::NET_DIAL, scope)]));
        let error = inherited_wild_caps(Verb::NET_DIAL, &typed)
            .expect_err("typed wildcard scopes are still unbounded");
        assert!(error.message.contains("unbounded"), "unexpected: {error}");
    }
}

#[test]
fn wildcard_need_still_works_for_resourceless_and_self_verbs() {
    let notify = delegation(CapSet::from_caps([Cap::new(Verb::UI_NOTIFY, Scope::Wild)]));
    let inherited = inherited_wild_caps(Verb::UI_NOTIFY, &notify)
        .expect("resourceless verbs have no narrower scope");
    assert_eq!(inherited.len(), 1);

    let spawn = delegation(CapSet::from_caps([Cap::new(Verb::PROC_SPAWN, Scope::Wild)]));
    let inherited = inherited_wild_caps(Verb::PROC_SPAWN, &spawn)
        .expect("self-referential verbs have no narrower scope");
    assert_eq!(inherited.len(), 1);
}

#[test]
fn wildcard_need_without_launcher_authority_is_refused() {
    let delegation = delegation(CapSet::new());
    let error =
        operation_caps(&test_app(), "scan", &[], &delegation).expect_err("nothing to inherit");
    assert!(error.message.contains("fs.meta"), "unexpected: {error}");
}

#[test]
fn missing_required_argument_is_refused_before_authorization() {
    let delegation = delegation(home_reader_ceiling());
    let error =
        operation_caps(&test_app(), "read", &[], &delegation).expect_err("path is required");
    assert!(error.message.contains("path"), "unexpected: {error}");
}

#[test]
fn unknown_operation_and_schema_probe_are_refused() {
    let delegation = delegation(home_reader_ceiling());
    assert!(operation_caps(&test_app(), "__schema__", &[], &delegation).is_err());
    assert!(operation_caps(&test_app(), "nope", &[], &delegation).is_err());
}

// ---------------------------------------------------------------------------
// Desktop launches
// ---------------------------------------------------------------------------

#[test]
fn gui_launch_must_name_the_declared_desktop_entrypoint() {
    let delegation = delegation(home_reader_ceiling());
    let app = test_app();
    assert!(gui_caps(&app, "--gui", &delegation).is_ok());

    let error = gui_caps(&app, "install", &delegation)
        .expect_err("an operation name is not a desktop entrypoint");
    assert!(error.message.contains("--gui"), "unexpected: {error}");

    let error = gui_caps(&app, "--anything-else", &delegation)
        .expect_err("an arbitrary label is not a desktop entrypoint");
    assert!(error.message.contains("--gui"), "unexpected: {error}");

    let error = gui_caps(&app_from(PKG_MANIFEST), "--gui", &delegation)
        .expect_err("an App without a desktop block has no GUI surface");
    assert!(error.message.contains("desktop"), "unexpected: {error}");
}

#[test]
fn gui_caps_skip_argument_bound_needs() {
    let delegation = delegation(unregistered_ceiling());
    let caps = gui_caps(&test_app(), "--gui", &delegation).expect("gui caps");
    assert_eq!(
        caps.verbs(),
        vec![Verb::FS_META],
        "a GUI launch has no arguments, so argument-bound needs stay unbound"
    );
    assert!(!caps.covers(&Cap::new(Verb::SYS_PACKAGE, Scope::name("vim"))));
}

#[test]
fn gui_wildcard_need_never_inherits_unbounded_authority() {
    let delegation = delegation(CapSet::from_caps([Cap::new(Verb::FS_META, Scope::Wild)]));
    let error = gui_caps(&test_app(), "--gui", &delegation)
        .expect_err("an unbounded ceiling must fail the whole GUI plan");
    assert!(error.message.contains("unbounded"), "unexpected: {error}");
}

#[test]
fn invoke_capability_requires_launcher_authority() {
    let _sandbox = approval_sandbox();
    let allowed = delegation(home_reader_ceiling());
    let caps = with_invoke_cap(CapSet::new(), "fs", &allowed).expect("invoke");
    assert!(caps.covers(&Cap::new(Verb::AGENT_INVOKE, Scope::name("fs"))));

    let denied = delegation(CapSet::new());
    assert!(with_invoke_cap(CapSet::new(), "pkg", &denied).is_err());
}

#[test]
fn app_session_tier_is_floored_at_worker() {
    assert_eq!(
        worker_floor(Some(Role::Admin.credential_tier())),
        Role::Worker.credential_tier()
    );
    assert_eq!(
        worker_floor(Some(Role::Observer.credential_tier())),
        Role::Observer.credential_tier()
    );
    assert_eq!(worker_floor(None), Role::Worker.credential_tier());
}

// ---------------------------------------------------------------------------
// Launch grants
// ---------------------------------------------------------------------------

fn test_authority() -> LauncherAuthority {
    let (pid, start_time_ticks) = this_process();
    LauncherAuthority {
        pid,
        start_time_ticks,
        parent: None,
        caps: home_reader_ceiling(),
        tier: None,
        scope: None,
        priority: None,
        role: None,
    }
}

fn test_client() -> ClientIdentity {
    let (pid, start_time_ticks) = this_process();
    ClientIdentity {
        pid: Some(pid),
        uid: Some(this_uid()),
        gid: Some(0),
        execution_uid: None,
        start_time_ticks,
        attended_local: false,
    }
}

#[cfg(unix)]
fn this_uid() -> u32 {
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn this_uid() -> u32 {
    0
}

#[test]
fn a_launch_grant_is_bound_to_its_session_and_launcher() {
    authority::authority().clear_for_test();
    let launcher = test_authority();
    let caps = home_reader_ceiling();
    let handle = issue_launch_grant(
        "app-grant-1",
        Some("power-manager"),
        None,
        this_uid(),
        &launcher,
        &caps,
        None,
    )
    .expect("mint a launch grant");

    require_launch_grant(&test_client(), &handle, "app-grant-1", this_uid())
        .expect("the launcher resolves its own grant");
    require_launch_grant(&test_client(), &handle, "app-other", this_uid())
        .expect_err("a handle does not cover another session");

    // A same-uid sibling that stole the characters cannot use them:
    // the grant is bound to the launcher process, not to possession.
    let mut sibling = test_client();
    sibling.pid = Some(1);
    require_launch_grant(&sibling, &handle, "app-grant-1", this_uid())
        .expect_err("a sibling process cannot exercise the grant");

    // Nor can another uid.
    require_launch_grant(
        &test_client(),
        &handle,
        "app-grant-1",
        this_uid().wrapping_add(1),
    )
    .expect_err("another owner cannot exercise the grant");

    require_launch_grant(&test_client(), &"a".repeat(64), "app-grant-1", this_uid())
        .expect_err("a guessed handle resolves nothing");
    authority::authority().clear_for_test();
}

#[test]
fn a_session_grant_is_derived_from_the_launch_grant_exactly_once() {
    authority::authority().clear_for_test();
    let launcher = test_authority();
    let caps = home_reader_ceiling();
    let handle = issue_launch_grant(
        "app-grant-2",
        Some("power-manager"),
        None,
        this_uid(),
        &launcher,
        &caps,
        None,
    )
    .expect("mint a launch grant");

    issue_session_grant(
        &handle,
        "app-grant-2",
        Some("power-manager"),
        this_uid(),
        std::process::id(),
        &caps,
        None,
    )
    .expect("bind derives the session grant");

    let error = issue_session_grant(
        &handle,
        "app-grant-2",
        Some("power-manager"),
        this_uid(),
        std::process::id(),
        &caps,
        None,
    )
    .expect_err("bind is one-shot");
    assert!(error.contains("ceiling"), "unexpected: {error}");
    authority::authority().clear_for_test();
}

#[test]
fn a_session_grant_cannot_widen_the_launch_grant() {
    authority::authority().clear_for_test();
    let launcher = test_authority();
    let handle = issue_launch_grant(
        "app-grant-3",
        Some("power-manager"),
        None,
        this_uid(),
        &launcher,
        &home_reader_ceiling(),
        None,
    )
    .expect("mint a launch grant");

    let mut wider = home_reader_ceiling();
    wider.insert(Cap::new(Verb::SYS_PACKAGE, Scope::name("nano")));
    let error = issue_session_grant(
        &handle,
        "app-grant-3",
        Some("power-manager"),
        this_uid(),
        std::process::id(),
        &wider,
        None,
    )
    .expect_err("a bind cannot mint authority the launcher never had");
    assert!(error.contains("widen"), "unexpected: {error}");
    authority::authority().clear_for_test();
}

#[test]
fn a_launch_grant_for_an_unverifiable_launcher_is_refused() {
    authority::authority().clear_for_test();
    let mut launcher = test_authority();
    // A pid that cannot be read from `/proc`: without a start time
    // there is no way to detect the pid being recycled.
    launcher.pid = u32::MAX - 1;
    launcher.start_time_ticks = Some(1);
    issue_launch_grant(
        "app-grant-4",
        Some("power-manager"),
        None,
        this_uid(),
        &launcher,
        &home_reader_ceiling(),
        None,
    )
    .expect_err("an unverifiable launcher gets no grant");
    authority::authority().clear_for_test();
}

// ---------------------------------------------------------------------------
// Request parsing
// ---------------------------------------------------------------------------

#[test]
fn launch_kind_is_a_closed_set() {
    assert!(launch_kind(&serde_json::json!({"kind": "operation"})).is_ok());
    assert!(launch_kind(&serde_json::json!({"kind": "gui"})).is_ok());
    assert!(launch_kind(&serde_json::json!({"kind": "mcp"})).is_ok());
    assert!(launch_kind(&serde_json::json!({"kind": "root"})).is_err());
    assert!(launch_kind(&serde_json::json!({})).is_err());
}

#[tokio::test]
async fn ordinary_user_process_cannot_mint_a_package_session_for_its_child() {
    let client = test_client();
    let app_error = register(serde_json::json!({}), &client).await.unwrap_err();
    assert!(app_error.message.contains("Extension Host"));
    let mcp_error = register_mcp(serde_json::json!({}), &client)
        .await
        .unwrap_err();
    assert!(mcp_error.contains("Extension Host"));
}

#[test]
fn only_app_session_commands_are_routed_here() {
    for command in COMMANDS {
        assert!(
            crate::clawd::routes::Command::parse(command).is_some(),
            "{command} must be a broker route"
        );
    }
    assert!(!COMMANDS.contains(&"system.package.control"));
    assert!(!COMMANDS.contains(&"task.submit"));
}

#[test]
fn argument_arrays_must_be_strings() {
    assert_eq!(
        string_array(&serde_json::json!({"args": ["a", "b"]}), "args").unwrap(),
        vec!["a".to_string(), "b".to_string()]
    );
    assert!(string_array(&serde_json::json!({}), "args")
        .unwrap()
        .is_empty());
    assert!(string_array(&serde_json::json!({"args": [1]}), "args").is_err());
    assert!(string_array(&serde_json::json!({"args": "a"}), "args").is_err());
}

// ---------------------------------------------------------------------------
// Transient capabilities, end to end
// ---------------------------------------------------------------------------
//
// Transient capabilities are the one place the daemon deliberately
// widens a running App, so the registry write and the re-derivation of
// the authority grant have to stay in step. If they can drift apart, a
// failed call leaves the registry wider than the authority — and
// `caps::require` inside the App, plus any later peer-session grant,
// would honour the leftover set.
//
// These drive the real thing: the owner-routed registry under
// `/run/cos/caps/<uid>/proc`, real grants in the authority store, and a
// real child process for the App. Preparing that partition is a
// root-only operation by design (`storage::ensure_routed_caps_dir`
// refuses otherwise), so each test reports and skips when it is not run
// as root rather than pretending to cover the path.

const E2E_UID: u32 = 0;

fn e2e_is_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn e2e_routed_registry() -> std::path::PathBuf {
    std::path::PathBuf::from("/run/cos/caps")
        .join(E2E_UID.to_string())
        .join("proc")
        .join("registry.json")
}

struct TransientHarness {
    _lock: std::sync::MutexGuard<'static, ()>,
    _data: tempfile::TempDir,
    prev_data: Option<std::ffi::OsString>,
    children: Vec<std::process::Child>,
}

impl Drop for TransientHarness {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(e2e_routed_registry());
        authority::authority().clear_for_test();
        match self.prev_data.take() {
            Some(value) => std::env::set_var("COS_DATA_DIR", value),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
    }
}

fn transient_harness() -> TransientHarness {
    let lock = crate::caps::test_env_lock::env_lock();
    let data = tempfile::tempdir().expect("tempdir");
    let prev_data = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_DATA_DIR", data.path());
    let _ = std::fs::remove_file(e2e_routed_registry());
    authority::authority().clear_for_test();
    TransientHarness {
        _lock: lock,
        _data: data,
        prev_data,
        children: Vec::new(),
    }
}

fn e2e_app_caps() -> CapSet {
    CapSet::from_caps([
        Cap::new(Verb::FS_READ, Scope::path("/root/**")),
        Cap::new(Verb::AGENT_INVOKE, Scope::name("fs")),
    ])
}

fn e2e_call_caps() -> CapSet {
    // Deliberately outside the App's base set, so "did the call scope
    // survive?" is a real question rather than one the base answers.
    CapSet::from_caps([Cap::new(Verb::FS_READ, Scope::path("/srv/scratch/**"))])
}

fn e2e_client() -> ClientIdentity {
    let (pid, start_time_ticks) = this_process();
    ClientIdentity {
        pid: Some(pid),
        uid: Some(E2E_UID),
        gid: Some(0),
        execution_uid: None,
        start_time_ticks,
        attended_local: false,
    }
}

fn e2e_row(session_id: &str, pid: u32, transient: Option<CapSet>) -> SessionInfo {
    SessionInfo {
        session_id: session_id.to_string(),
        pid,
        command: vec!["app".to_string()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("app".to_string()),
        parent: None,
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: Some(2),
        scope: None,
        priority: None,
        caps: Some(e2e_app_caps()),
        transient_caps: transient,
        role: Some(Role::Worker.name().to_string()),
        app_id: Some("fs".to_string()),
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
        client: crate::session::SessionClient::default(),
    }
}

fn e2e_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn e2e_install_row(info: SessionInfo) {
    e2e_runtime().block_on(crate::paths::with_user_override(
        E2E_UID,
        std::path::PathBuf::from("/root"),
        async move {
            crate::proc::register_session(info).expect("register routed session");
        },
    ));
}

fn e2e_read_transient(session_id: &str) -> Option<CapSet> {
    e2e_read_row(session_id).and_then(|row| row.transient_caps)
}

fn e2e_read_row(session_id: &str) -> Option<SessionInfo> {
    let id = session_id.to_string();
    e2e_runtime().block_on(crate::paths::with_user_override(
        E2E_UID,
        std::path::PathBuf::from("/root"),
        async move { crate::proc::session_info_by_id(&id) },
    ))
}

/// Mint the launch grant a launcher holds, and the session grant a
/// bound App runs under, exactly as `register` and `bind` do.
fn e2e_install_grants(session_id: &str, child_pid: u32) -> String {
    let (pid, ticks) = this_process();
    let launcher = LauncherAuthority {
        pid,
        start_time_ticks: ticks,
        parent: None,
        caps: e2e_app_caps(),
        tier: None,
        scope: None,
        priority: None,
        role: None,
    };
    let handle = issue_launch_grant(
        session_id,
        Some("fs"),
        None,
        E2E_UID,
        &launcher,
        &e2e_app_caps(),
        None,
    )
    .expect("launch grant");
    // Present only when the App process can be identified; the rollback
    // test deliberately uses a pid that cannot.
    let _ = issue_session_grant(
        &handle,
        session_id,
        Some("fs"),
        E2E_UID,
        child_pid,
        &e2e_app_caps(),
        None,
    );
    handle
}

fn e2e_spawn_child(harness: &mut TransientHarness) -> u32 {
    let child = std::process::Command::new("sleep")
        .arg("120")
        .spawn()
        .expect("spawn child");
    let pid = child.id();
    harness.children.push(child);
    pid
}

fn e2e_set_transient(handle: &str, session_id: &str) -> Result<Value, String> {
    e2e_runtime().block_on(set_transient(
        json!({"session_id": session_id, "handle": handle}),
        &e2e_client(),
    ))
}

fn e2e_session_grant_is_live(session_id: &str, pid: u32) -> bool {
    authority::authority()
        .resolve_session(
            session_id,
            &authority::Presentation {
                uid: E2E_UID,
                pid,
                start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
                audience: authority::Audience::SystemService,
                route: "test",
                session_id: Some(session_id.to_string()),
            },
        )
        .is_ok()
}

#[test]
fn gateway_target_grant_is_independent_of_caller_launch_ceiling() {
    if !e2e_is_root() {
        eprintln!("skipped: the routed capability partition can only be prepared as root");
        return;
    }
    let mut harness = transient_harness();
    let child = e2e_spawn_child(&mut harness);
    let session_id = "app-e2e-gateway-target";
    e2e_install_row(e2e_row(session_id, child, Some(e2e_call_caps())));

    issue_gateway_target_grant(
        session_id,
        "fs",
        E2E_UID,
        child,
        &e2e_call_caps(),
        crate::agentd::grant::now_ms() + 60_000,
        None,
    )
    .expect("Gateway target grant");
    let view = authority::authority()
        .resolve_session(
            session_id,
            &authority::Presentation {
                uid: E2E_UID,
                pid: child,
                start_time_ticks: crate::proc::read_start_time_ticks_pub(child),
                audience: authority::Audience::SystemService,
                route: "test",
                session_id: Some(session_id.to_string()),
            },
        )
        .expect("target session grant");
    assert_eq!(view.issuer, authority::Issuer::AppGateway);
    let required = Cap::new(Verb::FS_READ, Scope::path("/srv/scratch/**"));
    assert!(view.caps.covers(&required));
}

#[test]
fn clearing_a_call_scope_updates_registry_and_authority_together() {
    if !e2e_is_root() {
        eprintln!("skipped: the routed capability partition can only be prepared as root");
        return;
    }
    let mut harness = transient_harness();
    let child = e2e_spawn_child(&mut harness);
    let session_id = "app-e2e-clear";
    e2e_install_row(e2e_row(session_id, child, Some(e2e_call_caps())));
    let handle = e2e_install_grants(session_id, child);

    assert_eq!(e2e_read_transient(session_id), Some(e2e_call_caps()));
    e2e_set_transient(&handle, session_id).expect("clearing a call scope succeeds");

    assert_eq!(
        e2e_read_transient(session_id),
        None,
        "the registry must record the narrowed set"
    );
    assert!(
        e2e_session_grant_is_live(session_id, child),
        "the session grant is re-derived, not left revoked"
    );
    let caps = authority::authority()
        .resolve_session(
            session_id,
            &authority::Presentation {
                uid: E2E_UID,
                pid: child,
                start_time_ticks: crate::proc::read_start_time_ticks_pub(child),
                audience: authority::Audience::SystemService,
                route: "test",
                session_id: Some(session_id.to_string()),
            },
        )
        .expect("live grant")
        .caps;
    assert!(
        !caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/srv/scratch/x"))),
        "a cleared call scope must not survive in the authority"
    );
}

#[test]
fn a_failed_reissue_restores_the_previous_call_scope() {
    if !e2e_is_root() {
        eprintln!("skipped: the routed capability partition can only be prepared as root");
        return;
    }
    let _harness = transient_harness();
    let session_id = "app-e2e-rollback";
    // A pid nothing can be identified from: the registry write lands,
    // and re-deriving the grant then fails.
    let dead_pid = u32::MAX - 1;
    e2e_install_row(e2e_row(session_id, dead_pid, Some(e2e_call_caps())));
    let handle = e2e_install_grants(session_id, dead_pid);

    let error = e2e_set_transient(&handle, session_id).expect_err("re-deriving the grant fails");
    assert!(
        error.contains("could not be identified"),
        "unexpected: {error}"
    );
    assert_eq!(
        e2e_read_transient(session_id),
        Some(e2e_call_caps()),
        "a route error must leave the registry exactly as it found it"
    );
    assert!(
        !e2e_session_grant_is_live(session_id, std::process::id()),
        "no authority may outlive the transient state it was derived from"
    );
}

#[test]
fn a_session_that_disappeared_is_refused_before_anything_is_written() {
    if !e2e_is_root() {
        eprintln!("skipped: the routed capability partition can only be prepared as root");
        return;
    }
    let mut harness = transient_harness();
    let child = e2e_spawn_child(&mut harness);
    let session_id = "app-e2e-gone";
    e2e_install_row(e2e_row(session_id, child, Some(e2e_call_caps())));
    let handle = e2e_install_grants(session_id, child);

    let remove = session_id.to_string();
    e2e_runtime().block_on(crate::paths::with_user_override(
        E2E_UID,
        std::path::PathBuf::from("/root"),
        async move {
            crate::proc::deregister_session(&remove);
        },
    ));

    let error = e2e_set_transient(&handle, session_id).expect_err("a missing session is refused");
    assert!(
        error.contains("App session not found"),
        "unexpected: {error}"
    );
}

#[test]
fn an_unbound_session_cannot_be_re_scoped() {
    if !e2e_is_root() {
        eprintln!("skipped: the routed capability partition can only be prepared as root");
        return;
    }
    let _harness = transient_harness();
    let session_id = "app-e2e-unbound";
    let mut pending = e2e_row(session_id, 0, Some(e2e_call_caps()));
    pending.pending_bind = true;
    e2e_install_row(pending);
    let handle = e2e_install_grants(session_id, std::process::id());

    let error = e2e_set_transient(&handle, session_id).expect_err("an unbound session is refused");
    assert!(
        error.contains("not bound to a process"),
        "unexpected: {error}"
    );
    assert_eq!(
        e2e_read_transient(session_id),
        Some(e2e_call_caps()),
        "a refusal before the write leaves the registry untouched"
    );
}

#[test]
fn a_handle_for_another_session_cannot_re_scope_this_one() {
    if !e2e_is_root() {
        eprintln!("skipped: the routed capability partition can only be prepared as root");
        return;
    }
    let mut harness = transient_harness();
    let child = e2e_spawn_child(&mut harness);
    e2e_install_row(e2e_row("app-e2e-a", child, Some(e2e_call_caps())));
    let handle = e2e_install_grants("app-e2e-a", child);
    e2e_install_row(e2e_row("app-e2e-b", child, Some(e2e_call_caps())));

    let error = e2e_set_transient(&handle, "app-e2e-b").expect_err("cross-session use is refused");
    assert!(
        error.contains("does not cover this session"),
        "unexpected: {error}"
    );
    assert_eq!(
        e2e_read_transient("app-e2e-b"),
        Some(e2e_call_caps()),
        "a refused call writes nothing"
    );
}

#[test]
fn concurrent_calls_leave_the_registry_and_the_authority_agreeing() {
    if !e2e_is_root() {
        eprintln!("skipped: the routed capability partition can only be prepared as root");
        return;
    }
    let mut harness = transient_harness();
    let child = e2e_spawn_child(&mut harness);
    let session_id = "app-e2e-race";
    e2e_install_row(e2e_row(session_id, child, Some(e2e_call_caps())));
    let handle = e2e_install_grants(session_id, child);

    let mut threads = Vec::new();
    for _ in 0..4 {
        let handle = handle.clone();
        threads.push(std::thread::spawn(move || {
            e2e_set_transient(&handle, session_id).is_ok()
        }));
    }
    let outcomes: Vec<bool> = threads
        .into_iter()
        .map(|thread| thread.join().expect("join"))
        .collect();
    assert!(
        outcomes.iter().any(|ok| *ok),
        "at least one concurrent call must succeed"
    );
    assert_eq!(
        e2e_read_transient(session_id),
        None,
        "every successful call narrows to the same state"
    );
}

#[test]
fn a_re_scope_racing_a_teardown_never_strands_a_grant() {
    // The reachable interleaving with only route entry points: one
    // caller re-scopes while another tears the session down. Without a
    // per-session serializer the row can be removed between the
    // registry swap and the grant re-derivation, leaving a live grant
    // for a session that no longer exists — authority with nothing left
    // to bound it.
    //
    // Racing threads hit that window only by luck, so the exclusion is
    // proved directly: the test holds the session's serializer and
    // asserts each route blocks on it, then releases and asserts both
    // complete and agree. A route that skipped the lock would finish
    // while the guard is still held and fail the first assertion.
    if !e2e_is_root() {
        eprintln!("skipped: the routed capability partition can only be prepared as root");
        return;
    }
    let mut harness = transient_harness();
    let child = e2e_spawn_child(&mut harness);
    let session_id = "app-e2e-teardown-race";
    e2e_install_row(e2e_row(session_id, child, Some(e2e_call_caps())));
    let handle = e2e_install_grants(session_id, child);

    let blocker = session_lock(session_id);
    let guard = e2e_runtime().block_on(async { blocker.clone().lock_owned().await });

    let rescope_handle = handle.clone();
    let rescope =
        std::thread::spawn(move || e2e_set_transient(&rescope_handle, session_id).map(|_| ()));
    let teardown_handle = handle.clone();
    let teardown = std::thread::spawn(move || {
        e2e_runtime()
            .block_on(deregister(
                json!({"session_id": session_id, "handle": teardown_handle}),
                &e2e_client(),
            ))
            .map(|_| ())
    });

    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        !rescope.is_finished(),
        "a re-scope must wait for the session's transition lock"
    );
    assert!(
        !teardown.is_finished(),
        "a teardown must wait for the session's transition lock"
    );
    assert_eq!(
        e2e_read_transient(session_id),
        Some(e2e_call_caps()),
        "nothing may be written while another transition holds the session"
    );

    drop(guard);
    let rescope = rescope.join().expect("rescope");
    let teardown = teardown.join().expect("teardown");
    assert!(
        rescope.is_ok() || teardown.is_ok(),
        "one of the two transitions must land: {rescope:?} / {teardown:?}"
    );

    // Whichever order they landed in, the registry and the authority
    // must describe the same world.
    let row_exists = e2e_read_row(session_id).is_some();
    let grant_live = e2e_session_grant_is_live(session_id, child);
    assert!(
        !grant_live || row_exists,
        "a live grant outlived the session row it was derived from"
    );
    if row_exists {
        assert_eq!(
            e2e_read_transient(session_id),
            None,
            "a surviving row must not keep a call scope the grant lost"
        );
    }
}

#[test]
fn one_session_serializes_its_capability_transitions() {
    // Portable: the serializer is what makes the two halves of a
    // transition one transaction. Overlapping holders on one session
    // would be the registry/authority mismatch; blocking on *different*
    // sessions would make one App's tool call wait on another's.
    let runtime = e2e_runtime();
    runtime.block_on(async {
        let first = session_lock("serial-a");
        let held = first.lock().await;

        let same = session_lock("serial-a");
        assert!(
            same.try_lock().is_err(),
            "a second transition on the same session must wait"
        );

        let other = session_lock("serial-b");
        assert!(
            other.try_lock().is_ok(),
            "an unrelated session must not be blocked"
        );

        drop(held);
        assert!(
            session_lock("serial-a").try_lock().is_ok(),
            "the lock is released with the transition"
        );
    });

    release_session_lock("serial-a");
    release_session_lock("serial-b");
    assert!(
        !session_locks().lock().unwrap().contains_key("serial-a"),
        "a finished session leaves no entry behind"
    );
}

#[test]
fn a_serializer_someone_is_waiting_on_is_not_recycled() {
    // Dropping the entry while another caller still holds the old Arc
    // would hand the next caller a *different* mutex, and the two would
    // run concurrently on one session — the exact overlap the
    // serializer exists to prevent.
    //
    // At the point `deregister` collects the entry it holds its own
    // reference, so "idle" is the map plus that one. A second caller
    // waiting to run pushes the count above it.
    let deregistering_caller = session_lock("serial-busy");
    let a_waiting_caller = session_lock("serial-busy");
    release_session_lock("serial-busy");
    assert!(
        session_locks().lock().unwrap().contains_key("serial-busy"),
        "an entry another caller is waiting on stays put"
    );
    assert!(
        Arc::ptr_eq(&session_lock("serial-busy"), &a_waiting_caller),
        "the next caller must get the same mutex, not a fresh one"
    );

    drop(a_waiting_caller);
    release_session_lock("serial-busy");
    assert!(
        !session_locks().lock().unwrap().contains_key("serial-busy"),
        "once nobody else is waiting, the entry is collected"
    );
    drop(deregistering_caller);
}

#[test]
fn swapping_a_call_scope_returns_the_exact_previous_set() {
    // Portable: this is the primitive the transaction rolls back with,
    // and it must report what was there under the same lock that
    // replaced it, because a read-then-write could race another call.
    let _lock = crate::caps::test_env_lock::env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let previous_data = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_DATA_DIR", tmp.path());

    let session_id = format!("app-swap-{}", std::process::id());
    crate::proc::register_session(e2e_row(&session_id, std::process::id(), None))
        .expect("register");

    let first = crate::proc::swap_app_session_transient_caps(&session_id, Some(e2e_call_caps()))
        .expect("swap");
    assert_eq!(first, None, "the row started with no call scope");

    let replaced = CapSet::from_caps([Cap::new(Verb::FS_READ, Scope::path("/srv/other/**"))]);
    let second = crate::proc::swap_app_session_transient_caps(&session_id, Some(replaced.clone()))
        .expect("swap");
    assert_eq!(
        second,
        Some(e2e_call_caps()),
        "a replacement reports exactly the set it displaced"
    );

    let third = crate::proc::swap_app_session_transient_caps(&session_id, None).expect("swap");
    assert_eq!(third, Some(replaced));
    assert!(crate::proc::swap_app_session_transient_caps("app-missing", None).is_err());

    crate::proc::deregister_session(&session_id);
    match previous_data {
        Some(value) => std::env::set_var("COS_DATA_DIR", value),
        None => std::env::remove_var("COS_DATA_DIR"),
    }
}
