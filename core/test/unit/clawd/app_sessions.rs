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

/// Capability-plan tests operate on a manifest, not on a package on
/// disk, so the fixture records the quarantine state an unverified tree
/// would carry. `installed_app` — the authority path — never returns
/// one of these; it uses `apps::find_verified`.
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

// ---------------------------------------------------------------------------
// Relay authority
// ---------------------------------------------------------------------------

#[test]
fn only_session_scoped_system_service_routes_may_be_relayed() {
    for command in Command::ALL.iter().copied() {
        let route = command.route();
        let allowed = route.access == Access::User
            && route.authority.subject == authority::SubjectSource::Session
            && route.authority.audience == authority::Audience::SystemService
            && command != Command::AppSessionRelay;
        match relayable_route(command) {
            Ok(resolved) => {
                assert!(allowed, "{} should not be relayable", route.name);
                assert_eq!(resolved.name, route.name);
            }
            Err(error) => {
                assert!(!allowed, "{} should be relayable: {error}", route.name);
                assert!(error.contains(route.name), "{error}");
            }
        }
    }
}

#[test]
fn the_relay_route_cannot_relay_itself() {
    let error = match relayable_route(Command::AppSessionRelay) {
        Ok(route) => panic!("{} was relayable through itself", route.name),
        Err(error) => error,
    };
    assert!(error.contains("through itself"), "{error}");
}

#[test]
fn root_peer_and_handle_routes_are_never_relayable() {
    for command in Command::ALL.iter().copied() {
        let route = command.route();
        let refusable = route.access != Access::User
            || route.authority.subject != authority::SubjectSource::Session
            || route.authority.audience != authority::Audience::SystemService;
        if refusable {
            assert!(
                relayable_route(command).is_err(),
                "{} was relayable",
                route.name
            );
        }
    }
}

#[test]
fn a_relay_grant_is_bound_to_the_launcher_and_carries_no_capabilities() {
    if !e2e_is_root() {
        eprintln!("skipping: relay grant issuance needs the routed registry");
        return;
    }
    let mut harness = transient_harness();
    let session_id = "app-relay-issue";
    let child_pid = e2e_spawn_child(&mut harness);
    e2e_install_row(e2e_row(session_id, child_pid, None));
    let launch = e2e_install_grants(session_id, child_pid);

    let (launcher_pid, _) = this_process();
    let relay = issue_relay_grant(&launch, session_id, Some("fs"), E2E_UID, launcher_pid)
        .expect("relay grant");

    // Resolvable by this process, for this audience, with nothing in it.
    let view = authority::authority()
        .resolve(
            &relay,
            &authority::Presentation::new(
                E2E_UID,
                launcher_pid,
                crate::proc::read_start_time_ticks_pub(launcher_pid),
                authority::Audience::AppRelay,
                "test",
            ),
        )
        .expect("relay resolves for the launcher");
    assert_eq!(view.subject.session_id.as_deref(), Some(session_id));
    assert!(
        view.caps.is_empty(),
        "a relay grant carries no capabilities"
    );

    // Inert for another audience: it authorizes presenting, never acting.
    assert!(authority::authority()
        .resolve(
            &relay,
            &authority::Presentation::new(
                E2E_UID,
                launcher_pid,
                crate::proc::read_start_time_ticks_pub(launcher_pid),
                authority::Audience::SystemService,
                "test",
            ),
        )
        .is_err());

    // Inert in another process: a same-uid sibling or a process that
    // received the handle over a socket cannot use it.
    assert!(authority::authority()
        .resolve(
            &relay,
            &authority::Presentation::new(
                E2E_UID,
                child_pid,
                crate::proc::read_start_time_ticks_pub(child_pid),
                authority::Audience::AppRelay,
                "test",
            ),
        )
        .is_err());
}

#[test]
fn a_relayed_decision_carries_the_exact_live_session_authority() {
    if !e2e_is_root() {
        eprintln!("skipping: relayed authorization needs the routed registry");
        return;
    }
    let mut harness = transient_harness();
    let session_id = "app-relay-live";
    let child_pid = e2e_spawn_child(&mut harness);
    e2e_install_row(e2e_row(session_id, child_pid, None));
    let launch = e2e_install_grants(session_id, child_pid);
    let (launcher_pid, _) = this_process();
    let relay = issue_relay_grant(&launch, session_id, Some("fs"), E2E_UID, launcher_pid)
        .expect("relay grant");

    let route = Command::SystemNetworkControl.route();
    let params = json!({"session": session_id, "action": "status"});
    let decide = |handle: &str, session: &str| {
        e2e_runtime().block_on(authority::authorize_relayed(
            handle,
            session,
            route.name,
            &route.authority,
            &params,
            &e2e_client(),
        ))
    };

    // The launcher, holding the relay grant, presents the App session's
    // own authority — not its own.
    let decision = decide(&relay, session_id)
        .expect("relayed authorization")
        .expect("a session-scoped route resolves a grant");
    assert_eq!(decision.session_id(), Some(session_id));
    assert_eq!(decision.app_id(), Some("fs"));
    assert!(decision
        .caps()
        .covers(&Cap::new(Verb::FS_READ, Scope::path("/root/a.txt"))));
    // Base authority only: the call scope has not been installed.
    assert!(!decision
        .caps()
        .covers(&Cap::new(Verb::FS_READ, Scope::path("/srv/scratch/x"))));
    // And it spends against that session, not the launcher.
    let _authorized = decision
        .require(Cap::new(Verb::FS_READ, Scope::path("/root/a.txt")))
        .expect("exact capability spend");
    assert!(decision
        .require(Cap::new(Verb::FS_WRITE, Scope::path("/root/a.txt")))
        .is_err());

    // A transient call scope appears while it is installed …
    e2e_install_row(e2e_row(session_id, child_pid, Some(e2e_call_caps())));
    reissue_session_grant(
        &launch,
        session_id,
        Some("fs"),
        E2E_UID,
        child_pid,
        &{
            let mut caps = e2e_app_caps();
            caps.extend(e2e_call_caps().iter().cloned());
            caps
        },
        None,
    )
    .expect("reissue with call scope");
    let widened = decide(&relay, session_id)
        .expect("relayed authorization")
        .expect("decision");
    assert!(widened
        .caps()
        .covers(&Cap::new(Verb::FS_READ, Scope::path("/srv/scratch/x"))));

    // … and disappears the moment it is cleared.
    e2e_install_row(e2e_row(session_id, child_pid, None));
    reissue_session_grant(
        &launch,
        session_id,
        Some("fs"),
        E2E_UID,
        child_pid,
        &e2e_app_caps(),
        None,
    )
    .expect("clear call scope");
    let narrowed = decide(&relay, session_id)
        .expect("relayed authorization")
        .expect("decision");
    assert!(!narrowed
        .caps()
        .covers(&Cap::new(Verb::FS_READ, Scope::path("/srv/scratch/x"))));
}

#[test]
fn a_relay_is_refused_without_the_exact_handle_and_session() {
    if !e2e_is_root() {
        eprintln!("skipping: relayed authorization needs the routed registry");
        return;
    }
    let mut harness = transient_harness();
    let session_id = "app-relay-refuse";
    let other_id = "app-relay-other";
    let child_pid = e2e_spawn_child(&mut harness);
    let other_pid = e2e_spawn_child(&mut harness);
    e2e_install_row(e2e_row(session_id, child_pid, None));
    e2e_install_row(e2e_row(other_id, other_pid, None));
    let launch = e2e_install_grants(session_id, child_pid);
    let other_launch = e2e_install_grants(other_id, other_pid);
    let (launcher_pid, _) = this_process();
    let relay = issue_relay_grant(&launch, session_id, Some("fs"), E2E_UID, launcher_pid)
        .expect("relay grant");
    let other_relay = issue_relay_grant(&other_launch, other_id, Some("fs"), E2E_UID, launcher_pid)
        .expect("relay grant");

    let route = Command::SystemNetworkControl.route();
    let decide = |handle: &str, session: &str| {
        let params = json!({"session": session, "action": "status"});
        e2e_runtime().block_on(authority::authorize_relayed(
            handle,
            session,
            route.name,
            &route.authority,
            &params,
            &e2e_client(),
        ))
    };

    // No handle, a made-up handle, and a handle for a different session
    // are all refused; so is naming another session with a valid one.
    assert!(decide("", session_id).is_err());
    assert!(decide("deadbeef", session_id).is_err());
    assert!(decide(&other_relay, session_id).is_err());
    assert!(decide(&relay, other_id).is_err());

    // A revoked launch — the shape `deregister` produces — takes the
    // relay with it.
    decide(&relay, session_id).expect("relay works before teardown");
    authority::authority().revoke_session(session_id);
    crate::clawd::authority::revoke_session(session_id);
    assert!(
        decide(&relay, session_id).is_err(),
        "a relay outlived its session"
    );
}
#[test]
fn a_relay_proof_skips_only_the_cgroup_comparison() {
    if !e2e_is_root() {
        eprintln!("skipping: relay presentation needs the routed registry");
        return;
    }
    let mut harness = transient_harness();
    let session_id = "app-relay-audience";
    let child_pid = e2e_spawn_child(&mut harness);
    e2e_install_row(e2e_row(session_id, child_pid, None));
    let launch = e2e_install_grants(session_id, child_pid);
    let (launcher_pid, _) = this_process();
    let relay = issue_relay_grant(&launch, session_id, Some("fs"), E2E_UID, launcher_pid)
        .expect("relay grant");

    // The session grant carries SystemService and Credential. A relay
    // proof lets the launcher *present* it; it does not add an audience
    // the grant never had, and it does not let another session's id
    // ride along.
    let route = Command::SystemNetworkControl.route();
    let params = json!({"session": session_id, "action": "status"});
    e2e_runtime()
        .block_on(authority::authorize_relayed(
            &relay,
            session_id,
            route.name,
            &route.authority,
            &params,
            &e2e_client(),
        ))
        .expect("system-service audience is inside the session grant");

    // Re-declaring the inner route in an audience the session grant
    // does not carry must fail even though the relay proof is valid and
    // the outer AppRelay filter already passed.
    static SCHEDULER_AUDIENCE: authority::RouteAuthority = authority::RouteAuthority {
        audience: authority::Audience::Scheduler,
        subject: authority::SubjectSource::Session,
        requirement: authority::route_derived,
        approval: authority::Approval::Ineligible,
        transient: authority::TransientCaps::Excluded,
    };
    let masked = e2e_runtime().block_on(authority::authorize_relayed(
        &relay,
        session_id,
        route.name,
        &SCHEDULER_AUDIENCE,
        &params,
        &e2e_client(),
    ));
    assert!(
        masked.is_err(),
        "an inner audience outside the session grant was masked by the relay"
    );

    // Presenting the relay for a session the grant does not name is
    // refused by the store's subject check, not only by the route.
    let proof_for_other = authority::authority().resolve_session(
        session_id,
        &authority::Presentation {
            uid: E2E_UID,
            pid: launcher_pid,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(launcher_pid),
            audience: authority::Audience::SystemService,
            route: "test",
            session_id: Some("app-somebody-else".to_string()),
        },
    );
    assert!(
        proof_for_other.is_err(),
        "a mismatched subject was accepted"
    );
}

#[test]
fn the_outer_relay_audience_is_distinct_from_the_inner_one() {
    // The relay route is reached with AppRelay; every route it may
    // forward is decided with SystemService. Collapsing the two would
    // let a relay grant act directly on a provider.
    let relay_route = Command::AppSessionRelay.route();
    assert_eq!(
        relay_route.authority.audience,
        authority::Audience::AppRelay
    );
    assert_eq!(
        relay_route.authority.subject,
        authority::SubjectSource::Handle
    );
    for command in Command::ALL.iter().copied() {
        if let Ok(route) = relayable_route(command) {
            assert_eq!(
                route.authority.audience,
                authority::Audience::SystemService,
                "{} is relayable in the wrong audience",
                route.name
            );
            assert_ne!(route.authority.audience, authority::Audience::AppRelay);
        }
    }
}
// ---------------------------------------------------------------------------
// The provenance ceiling is enforced by the daemon, not by the launcher
// ---------------------------------------------------------------------------
//
// A launcher is unprivileged local code. It applies the same ceiling
// before it builds a sandbox, but the question these tests answer is
// what happens when that copy is wrong, absent or hostile: does `clawd`
// still refuse to *grant* a developer-trusted package anything above its
// tier? Every assertion below therefore goes through the real
// authorization funnel and the real authority store, never through
// `bridge`.

/// An unsigned App that asks for everything the developer tier forbids.
const DEV_MANIFEST: &str = r#"{
  "id": "scratch",
  "version": "0.1.0",
  "name": "Scratch",
  "desktop": {"exec": "--gui"},
  "operations": {
    "grab": {
      "label": "Grab",
      "args": [],
      "needs": [
        {"verb": "sys.package",
         "scope": {"kind": "fixed", "scope": {"kind": "name", "value": "nano"}},
         "why": "install"},
        {"verb": "secret.read",
         "scope": {"kind": "fixed", "scope": {"kind": "name", "value": "default/TOKEN"}},
         "why": "auth"},
        {"verb": "net.dial",
         "scope": {"kind": "fixed", "scope": {"kind": "host", "value": "evil.example"}},
         "why": "sync"},
        {"verb": "proc.spawn", "scope": {"kind": "wild"}, "why": "helper"},
        {"verb": "fs.exec",
         "scope": {"kind": "fixed", "scope": {"kind": "path", "value": "/usr/bin/**"}},
         "why": "run"},
        {"verb": "fs.read",
         "scope": {"kind": "fixed", "scope": {"kind": "path", "value": "/etc/**"}},
         "why": "config"},
        {"verb": "fs.meta", "scope": {"kind": "wild"}, "why": "list"},
        {"verb": "agent.spawn", "scope": {"kind": "wild"}, "why": "delegate"},
        {"verb": "ui.notify", "scope": {"kind": "wild"}, "why": "tell the user"},
        {"verb": "data.kv.write",
         "scope": {"kind": "fixed", "scope": {"kind": "name", "value": "scratch"}},
         "why": "state"}
      ]
    }
  },
  "session": {
    "tools": [
      {
        "name": "escalate",
        "summary": "widen after launch",
        "args": [],
        "needs": [
          {"verb": "sys.package",
           "scope": {"kind": "fixed", "scope": {"kind": "name", "value": "nano"}},
           "why": "install"},
          {"verb": "data.kv.read",
           "scope": {"kind": "fixed", "scope": {"kind": "name", "value": "scratch"}},
           "why": "state"}
        ]
      }
    ]
  }
}"#;

fn dev_app() -> App {
    app_from(DEV_MANIFEST)
}

fn developer_ceiling() -> Ceiling {
    Ceiling::for_package(crate::provenance::TrustTier::Developer, "scratch")
}

fn vendor_ceiling() -> Ceiling {
    Ceiling::for_tier(crate::provenance::TrustTier::Vendor)
}

/// A launcher that holds every verb in the catalog at the widest scope
/// a wildcard need is allowed to inherit, so nothing below is denied
/// merely because the parent could not delegate it.
///
/// Resource-addressed verbs get a *bounded* scope on purpose:
/// `inherited_wild_caps` refuses to expand a `wild` need from an
/// unbounded parent scope, and that refusal is a different rule from
/// the one under test.
fn omnipotent_delegation() -> Delegation {
    let mut caps = CapSet::new();
    for meta in crate::caps::catalog::CATALOG {
        let scope = match meta.scope_kind {
            ScopeKind::Path => Scope::path("/srv/**"),
            ScopeKind::Host => Scope::host("*.example"),
            ScopeKind::Name => Scope::name("a*"),
            ScopeKind::SelfRef | ScopeKind::Wild | ScopeKind::None => Scope::Wild,
        };
        caps.insert(Cap::new(meta.verb, scope));
    }
    // Plus the exact scopes the fixture manifest names, so its
    // non-wildcard needs are delegable rather than merely "missing".
    caps.extend([
        Cap::new(Verb::SYS_PACKAGE, Scope::name("nano")),
        Cap::new(Verb::SECRET_READ, Scope::name("default/TOKEN")),
        Cap::new(Verb::NET_DIAL, Scope::host("evil.example")),
        Cap::new(Verb::FS_EXEC, Scope::path("/usr/bin/**")),
        Cap::new(Verb::FS_READ, Scope::path("/etc/**")),
        Cap::new(Verb::FS_READ, Scope::path("/root/scratch/**")),
        Cap::new(Verb::DATA_KV_WRITE, Scope::name("scratch")),
        Cap::new(Verb::DATA_KV_READ, Scope::name("scratch")),
    ]);
    delegation(caps)
}

const FORBIDDEN_FOR_DEVELOPER: &[(Verb, &str)] = &[
    (Verb::SYS_PACKAGE, "sys.package"),
    (Verb::SECRET_READ, "secret.read"),
    (Verb::NET_DIAL, "net.dial"),
    (Verb::PROC_SPAWN, "proc.spawn"),
    (Verb::FS_EXEC, "fs.exec"),
    (Verb::AGENT_SPAWN, "agent.spawn"),
];

fn assert_within_developer_ceiling(caps: &CapSet) {
    for cap in caps.iter() {
        assert!(
            developer_ceiling().allows_cap(cap),
            "clawd granted `{}` to developer-trusted content",
            cap.verb.as_str()
        );
    }
    for (verb, name) in FORBIDDEN_FOR_DEVELOPER {
        assert!(
            !caps.iter().any(|cap| cap.verb == *verb),
            "`{name}` survived into a developer grant"
        );
    }
    for cap in caps.iter() {
        assert!(
            !cap.scope.is_wildcard()
                || !crate::provenance::ceiling::verb_addresses_a_resource(cap.verb),
            "a wildcard `{}` scope survived into a developer grant",
            cap.verb.as_str()
        );
    }
}

#[test]
fn the_daemon_clamps_a_developer_operation_plan_before_it_becomes_authority() {
    let app = dev_app();
    let delegation = omnipotent_delegation();
    let plan = operation_plan(&app, "grab", &[], &delegation, &publisher_ceiling()).expect("plan");

    // The launcher could delegate every one of these, so the plan the
    // daemon starts from really does contain them.
    assert!(plan
        .caps
        .covers(&Cap::new(Verb::SYS_PACKAGE, Scope::name("nano"))));
    assert!(plan
        .caps
        .covers(&Cap::new(Verb::SECRET_READ, Scope::name("default/TOKEN"))));

    let granted = authorize_plan(&delegation, plan, &developer_ceiling(), "scratch")
        .expect("a clamped plan still authorizes");
    assert_within_developer_ceiling(&granted);

    // The benign allow-list survives exactly.
    assert!(granted.iter().any(|cap| cap.verb == Verb::UI_NOTIFY));
    assert!(granted.covers(&Cap::new(Verb::DATA_KV_WRITE, Scope::name("scratch"))));
    assert!(granted.covers(&Cap::new(Verb::FS_READ, Scope::path("/etc/**"))));
}

#[test]
fn a_signed_package_keeps_everything_the_same_plan_asked_for() {
    let app = dev_app();
    let delegation = omnipotent_delegation();

    for ceiling in [publisher_ceiling(), vendor_ceiling()] {
        let plan = operation_plan(&app, "grab", &[], &delegation, &ceiling).expect("plan");
        let granted =
            authorize_plan(&delegation, plan, &ceiling, "scratch").expect("signed content");
        for (verb, name) in FORBIDDEN_FOR_DEVELOPER {
            assert!(
                granted.iter().any(|cap| cap.verb == *verb),
                "`{name}` must survive for {} content",
                ceiling.label()
            );
        }
    }
}

#[test]
fn a_developer_gui_launch_is_clamped_on_the_same_path() {
    let app = dev_app();
    let delegation = omnipotent_delegation();
    let signed = gui_plan(&app, "--gui", &delegation, &publisher_ceiling()).expect("gui plan");
    assert!(signed
        .caps
        .covers(&Cap::new(Verb::SYS_PACKAGE, Scope::name("nano"))));

    let plan = gui_plan(&app, "--gui", &delegation, &developer_ceiling()).expect("gui plan");
    let granted = authorize_plan(&delegation, plan, &developer_ceiling(), "scratch")
        .expect("gui authorization");
    assert_within_developer_ceiling(&granted);
}

#[test]
fn a_developer_session_tool_cannot_widen_after_launch() {
    let app = dev_app();
    let delegation = omnipotent_delegation();
    let call = json!({"tool": "escalate"});
    let signed = session_tool_plan(&app, &call, &delegation, &publisher_ceiling())
        .expect("session tool plan");
    assert!(signed
        .caps
        .covers(&Cap::new(Verb::SYS_PACKAGE, Scope::name("nano"))));

    let plan = session_tool_plan(&app, &call, &delegation, &developer_ceiling())
        .expect("session tool plan");
    let granted = authorize_plan(&delegation, plan, &developer_ceiling(), "scratch")
        .expect("transient authorization");
    assert_within_developer_ceiling(&granted);
    assert!(
        granted.covers(&Cap::new(Verb::DATA_KV_READ, Scope::name("scratch"))),
        "the benign half of the call must still be granted"
    );
}

#[test]
fn a_wider_parent_cannot_lift_a_developer_package() {
    let app = dev_app();
    // Two launchers: one that can delegate everything, and one that
    // holds only the resourceless verbs the manifest asks for wild —
    // so the plan still builds and everything interesting lands in
    // `missing`, where consent would otherwise be able to grant it.
    let narrow = delegation(CapSet::from_caps([
        Cap::new(Verb::DATA_KV_WRITE, Scope::name("scratch")),
        Cap::new(Verb::PROC_SPAWN, Scope::Wild),
        Cap::new(Verb::AGENT_SPAWN, Scope::Wild),
        Cap::new(Verb::UI_NOTIFY, Scope::Wild),
    ]));
    let wide = omnipotent_delegation();

    let wide_granted = authorize_plan(
        &wide,
        operation_plan(&app, "grab", &[], &wide, &developer_ceiling()).expect("plan"),
        &developer_ceiling(),
        "scratch",
    )
    .expect("wide launcher");
    assert_within_developer_ceiling(&wide_granted);

    // The narrow launcher is short several capabilities. Whatever
    // reaches the approvals store must already be inside the ceiling,
    // so a user consenting to a prompt can never reintroduce a
    // forbidden verb.
    let plan = operation_plan(&app, "grab", &[], &narrow, &developer_ceiling()).expect("plan");
    let (kept, dropped) = developer_ceiling().clamp_vec(&plan.missing);
    assert!(
        !dropped.is_empty(),
        "the fixture must actually ask for something forbidden"
    );
    for cap in &kept {
        assert!(
            !FORBIDDEN_FOR_DEVELOPER
                .iter()
                .any(|(verb, _)| cap.verb == *verb),
            "a forbidden capability reached the approvals store as `{}`",
            cap.verb.as_str()
        );
    }
}

#[test]
fn a_developer_package_never_inherits_a_wild_scope_binding() {
    let app = dev_app();
    let delegation = omnipotent_delegation();

    // Signed content borrows the launcher's bounded `fs.meta` reach …
    let signed =
        operation_plan(&app, "grab", &[], &delegation, &publisher_ceiling()).expect("plan");
    assert!(signed
        .caps
        .covers(&Cap::new(Verb::FS_META, Scope::path("/srv/x"))));

    // … and unsigned content borrows none of the launcher's reach over
    // a real resource namespace.
    let plan = operation_plan(&app, "grab", &[], &delegation, &developer_ceiling()).expect("plan");
    assert!(
        !plan.caps.iter().any(|cap| cap.verb == Verb::FS_META),
        "a wild need over a resource must not expand for developer content"
    );

    // `proc.spawn` and `agent.spawn` address no resource namespace, so
    // the *binding* is not what stops them — the allow-list is, one
    // step later, where authority is actually granted.
    let granted =
        authorize_plan(&delegation, plan, &developer_ceiling(), "scratch").expect("authorization");
    assert!(!granted.iter().any(|cap| cap.verb == Verb::PROC_SPAWN));
    assert!(!granted.iter().any(|cap| cap.verb == Verb::AGENT_SPAWN));

    // A resourceless verb on the allow-list has no narrower scope than
    // wild, so it is not an inheritance and survives.
    assert!(granted.iter().any(|cap| cap.verb == Verb::UI_NOTIFY));
}

#[test]
fn a_developer_launch_grant_reaches_only_the_launch_audience() {
    authority::authority().clear_for_test();
    let launcher = test_authority();
    let caps = CapSet::from_caps([Cap::new(Verb::DATA_KV_WRITE, Scope::name("scratch"))]);
    let handle = issue_launch_grant(
        "app-dev-1",
        Some("scratch"),
        this_uid(),
        &launcher,
        &caps,
        Some(&developer_ceiling()),
    )
    .expect("mint a developer launch grant");

    // The launcher can still drive the launch lifecycle …
    require_launch_grant(&test_client(), &handle, "app-dev-1", this_uid())
        .expect("app-launch audience survives");

    // … and nothing else. These are the audiences that address a
    // privileged provider route.
    for audience in [
        authority::Audience::SystemService,
        authority::Audience::Credential,
        authority::Audience::AppRelay,
    ] {
        let error = authority::authority()
            .resolve(
                &handle,
                &authority::Presentation::new(
                    this_uid(),
                    std::process::id(),
                    this_process().1,
                    audience,
                    "test",
                ),
            )
            .expect_err("a developer grant must not reach a privileged audience");
        let _ = error;
    }
    authority::authority().clear_for_test();
}

#[test]
fn a_signed_launch_grant_still_reaches_every_audience() {
    authority::authority().clear_for_test();
    let launcher = test_authority();
    let caps = home_reader_ceiling();
    let handle = issue_launch_grant(
        "app-dev-2",
        Some("fs"),
        this_uid(),
        &launcher,
        &caps,
        Some(&publisher_ceiling()),
    )
    .expect("mint a signed launch grant");
    for audience in [
        authority::Audience::AppLaunch,
        authority::Audience::SystemService,
        authority::Audience::Credential,
        authority::Audience::AppRelay,
    ] {
        authority::authority()
            .resolve(
                &handle,
                &authority::Presentation::new(
                    this_uid(),
                    std::process::id(),
                    this_process().1,
                    audience,
                    "test",
                ),
            )
            .unwrap_or_else(|error| panic!("{} should resolve: {error}", audience.as_str()));
    }
    authority::authority().clear_for_test();
}

#[test]
fn a_developer_session_grant_addresses_no_provider_route() {
    authority::authority().clear_for_test();
    let launcher = test_authority();
    let ceiling = developer_ceiling();
    let caps = CapSet::from_caps([Cap::new(Verb::DATA_KV_WRITE, Scope::name("scratch"))]);
    let handle = issue_launch_grant(
        "app-dev-3",
        Some("scratch"),
        this_uid(),
        &launcher,
        &caps,
        Some(&ceiling),
    )
    .expect("launch grant");
    issue_session_grant(
        &handle,
        "app-dev-3",
        Some("scratch"),
        this_uid(),
        std::process::id(),
        &caps,
        Some(&ceiling),
    )
    .expect("session grant");

    // The row exists — the App is a real session — but every audience a
    // provider route is addressed by is refused.
    for audience in [
        authority::Audience::SystemService,
        authority::Audience::Credential,
        authority::Audience::AppRelay,
    ] {
        authority::authority()
            .resolve_session(
                "app-dev-3",
                &authority::Presentation {
                    uid: this_uid(),
                    pid: std::process::id(),
                    start_time_ticks: this_process().1,
                    audience,
                    route: "test",
                    session_id: Some("app-dev-3".to_string()),
                },
            )
            .expect_err("a developer session grant must reach no provider audience");
    }
    authority::authority().clear_for_test();
}

#[test]
fn a_developer_package_is_never_issued_a_relay_grant() {
    authority::authority().clear_for_test();
    let launcher = test_authority();
    let ceiling = developer_ceiling();
    let handle = issue_launch_grant(
        "app-dev-4",
        Some("scratch"),
        this_uid(),
        &launcher,
        &CapSet::new(),
        Some(&ceiling),
    )
    .expect("launch grant");

    // `bind` skips this call for developer content; if a future change
    // ever calls it anyway, the attenuation itself must refuse, because
    // the parent grant does not carry the relay audience.
    assert!(!ceiling.allows_relay());
    issue_relay_grant(
        &handle,
        "app-dev-4",
        Some("scratch"),
        this_uid(),
        std::process::id(),
    )
    .expect_err("a developer launch grant cannot be attenuated into a relay");
    authority::authority().clear_for_test();
}

#[test]
fn every_audience_is_classified_for_the_provenance_ceiling() {
    // `audience_facet` is exhaustive by construction; this pins the
    // mapping so a renamed variant cannot quietly become permissive.
    for (audience, expected) in [
        (
            authority::Audience::AppLaunch,
            crate::provenance::ceiling::Audience::AppLaunch,
        ),
        (
            authority::Audience::AppRelay,
            crate::provenance::ceiling::Audience::AppRelay,
        ),
        (
            authority::Audience::SystemService,
            crate::provenance::ceiling::Audience::SystemService,
        ),
        (
            authority::Audience::Credential,
            crate::provenance::ceiling::Audience::Credential,
        ),
        (
            authority::Audience::Scheduler,
            crate::provenance::ceiling::Audience::Scheduler,
        ),
        (
            authority::Audience::Permission,
            crate::provenance::ceiling::Audience::Permission,
        ),
        (
            authority::Audience::Transaction,
            crate::provenance::ceiling::Audience::Transaction,
        ),
        (
            authority::Audience::Context,
            crate::provenance::ceiling::Audience::Context,
        ),
        (
            authority::Audience::Notification,
            crate::provenance::ceiling::Audience::Notification,
        ),
        (
            authority::Audience::Task,
            crate::provenance::ceiling::Audience::Task,
        ),
        (
            authority::Audience::Daemon,
            crate::provenance::ceiling::Audience::Daemon,
        ),
    ] {
        assert_eq!(audience_facet(audience), expected);
        assert_eq!(audience.as_str(), expected.as_str());
    }

    // And the production filter really is a filter.
    let requested = [
        authority::Audience::AppLaunch,
        authority::Audience::SystemService,
        authority::Audience::Credential,
        authority::Audience::AppRelay,
    ];
    let developer = permitted_audiences(Some(&developer_ceiling()), &requested);
    assert!(developer.contains(authority::Audience::AppLaunch));
    for audience in [
        authority::Audience::SystemService,
        authority::Audience::Credential,
        authority::Audience::AppRelay,
    ] {
        assert!(!developer.contains(audience));
    }
    let signed = permitted_audiences(Some(&publisher_ceiling()), &requested);
    let unbounded = permitted_audiences(None, &requested);
    for audience in requested {
        assert!(signed.contains(audience));
        assert!(unbounded.contains(audience));
    }
}

// ---------------------------------------------------------------------------
// End to end, as root, through the real routes
// ---------------------------------------------------------------------------
//
// The tests above prove the funnel clamps. These prove the funnel is
// the only way in: a real dev-trusted package on disk, registered and
// bound through `register`/`bind`, with the routed registry row, the
// live authority grant and the relay handle all read back afterwards.

/// The dangerous half is what a hostile unsigned package would ask for;
/// the benign half is what the developer tier is meant to allow.
const E2E_DEV_MANIFEST: &str = r#"{
  "id": "scratch",
  "version": "0.1.0",
  "name": "Scratch",
  "operations": {
    "run": {
      "label": "Run",
      "args": [],
      "needs": [
        {"verb": "sys.package",
         "scope": {"kind": "fixed", "scope": {"kind": "name", "value": "nano"}},
         "why": "install"},
        {"verb": "secret.read",
         "scope": {"kind": "fixed", "scope": {"kind": "name", "value": "default/TOKEN"}},
         "why": "auth"},
        {"verb": "net.dial",
         "scope": {"kind": "fixed", "scope": {"kind": "host", "value": "evil.example"}},
         "why": "sync"},
        {"verb": "proc.spawn", "scope": {"kind": "wild"}, "why": "helper"},
        {"verb": "fs.exec",
         "scope": {"kind": "fixed", "scope": {"kind": "path", "value": "/usr/bin/**"}},
         "why": "run"},
        {"verb": "fs.meta", "scope": {"kind": "wild"}, "why": "list"},
        {"verb": "agent.spawn", "scope": {"kind": "wild"}, "why": "delegate"},
        {"verb": "fs.read",
         "scope": {"kind": "fixed", "scope": {"kind": "path", "value": "/root/scratch/**"}},
         "why": "its own tree"},
        {"verb": "data.kv.write",
         "scope": {"kind": "fixed", "scope": {"kind": "name", "value": "scratch"}},
         "why": "state"},
        {"verb": "ui.notify", "scope": {"kind": "wild"}, "why": "tell the user"}
      ]
    }
  }
}"#;

/// Install an unsigned App under an owner developer-trust grant and
/// point the process at it. Returns the apps root, which the caller
/// keeps alive for the length of the test.
#[cfg(unix)]
fn e2e_install_dev_app(manifest: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    use crate::provenance::{sign, trust::TrustRootSpec, PackageKind, TrustStore, TrustTier};

    let root = crate::test_env::secure_scratch_dir("clawd-dev-app");
    let apps = root.join("apps");
    let dir = apps.join("scratch");
    std::fs::create_dir_all(&dir).expect("app dir");
    std::fs::write(dir.join("app.json"), manifest).expect("manifest");
    std::fs::write(dir.join("main.py"), "print('hi')\n").expect("entrypoint");

    let body = sign::build_body(
        &dir,
        &sign::SignRequest {
            kind: PackageKind::App,
            id: "scratch".to_string(),
            version: "0.1.0".to_string(),
            manifest_schema: "developer".to_string(),
            manifest_path: "app.json".to_string(),
            entrypoints: vec!["main.py".to_string()],
            resources: vec![],
        },
    )
    .expect("build content manifest");
    let digest = crate::provenance::envelope::content_digest(&body.files);

    let dev_root = root.join("devtrust");
    std::fs::create_dir_all(&dev_root).expect("dev trust root");
    let grants = json!({
        "schema": crate::provenance::trust::DEV_TRUST_SCHEMA_V1,
        "grants": [{
            "kind": "app",
            "id": "scratch",
            "path": dir.canonicalize().expect("canonical app dir"),
            "content_digest": digest,
            "granted_at": "2026-01-01T00:00:00Z",
        }],
    });
    let grants_path = dev_root.join("grants.json");
    std::fs::write(&grants_path, serde_json::to_vec_pretty(&grants).unwrap()).expect("grants");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&grants_path, std::fs::Permissions::from_mode(0o600))
            .expect("grant mode");
    }

    let roots = vec![TrustRootSpec {
        path: dev_root,
        tier: TrustTier::Developer,
        allowed_uids: vec![crate::provenance::fsec::effective_uid()],
        domain: crate::provenance::state::TrustDomain::Owner(
            crate::provenance::fsec::effective_uid(),
        ),
    }];
    // Trust files with no recorded generation fail the domain closed,
    // so the fixture records it exactly as `cos provenance dev-trust`
    // does.
    crate::test_env::record_trust_state(&roots);
    crate::provenance::set_trust_store_for_roots(TrustStore::load_roots(&roots), roots);
    std::env::set_var("COS_APPS_DIR", &apps);
    (root, apps)
}

/// A registered non-App parent session for this very process, holding
/// every capability the fixture asks for. Without it the launcher falls
/// back to the daemon's home-bounded policy and the interesting caps
/// would be refused for the wrong reason.
#[cfg(unix)]
fn e2e_install_parent_session(session_id: &str) {
    let (pid, ticks) = this_process();
    let mut caps = omnipotent_delegation().ceiling;
    caps.insert(Cap::new(Verb::AGENT_INVOKE, Scope::name("scratch")));
    caps.insert(Cap::new(Verb::FS_READ, Scope::path("/root/scratch/**")));
    let info = SessionInfo {
        session_id: session_id.to_string(),
        pid,
        command: vec!["cos".to_string()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("cli".to_string()),
        parent: None,
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: Some(1),
        scope: None,
        priority: None,
        caps: Some(caps),
        transient_caps: None,
        role: Some(Role::Automator.name().to_string()),
        app_id: None,
        pending_bind: false,
        start_time_ticks: ticks,
        client: crate::session::SessionClient::default(),
    };
    e2e_install_row(info);
}

#[cfg(unix)]
#[test]
fn a_dev_trusted_app_is_registered_and_bound_with_no_privileged_authority() {
    if !e2e_is_root() {
        eprintln!("skipped: the routed capability partition can only be prepared as root");
        return;
    }
    let mut harness = transient_harness();
    let previous_apps = std::env::var_os("COS_APPS_DIR");
    let (root, _apps) = e2e_install_dev_app(E2E_DEV_MANIFEST);
    e2e_install_parent_session("cli-parent");

    let result = e2e_runtime()
        .block_on(register(
            json!({"app_id": "scratch", "kind": "operation", "operation": "run", "args": []}),
            &e2e_client(),
        ))
        .expect("a dev-trusted App still launches");

    // 1. What the daemon told the launcher it granted.
    let granted: CapSet =
        serde_json::from_value(result["caps"].clone()).expect("clawd reports its own grant");
    assert_eq!(result["trust_tier"], "developer");
    assert_within_developer_ceiling(&granted);
    assert!(granted.covers(&Cap::new(Verb::DATA_KV_WRITE, Scope::name("scratch"))));
    assert!(granted.covers(&Cap::new(Verb::AGENT_INVOKE, Scope::name("scratch"))));

    let session_id = result["session_id"]
        .as_str()
        .expect("session id")
        .to_string();
    let handle = result["handle"].as_str().expect("handle").to_string();

    // 2. What the routed registry row records — this is what
    //    `caps::require` inside the App reads.
    let row = e2e_read_row(&session_id).expect("session row");
    let row_caps = row.caps.clone().expect("row caps");
    assert_within_developer_ceiling(&row_caps);
    assert_eq!(row_caps, granted, "the row and the response must agree");

    // 3. The launch grant reaches only the launch audience.
    for audience in [
        authority::Audience::SystemService,
        authority::Audience::Credential,
        authority::Audience::AppRelay,
    ] {
        authority::authority()
            .resolve(
                &handle,
                &authority::Presentation::new(
                    E2E_UID,
                    std::process::id(),
                    this_process().1,
                    audience,
                    "test",
                ),
            )
            .expect_err("a developer launch grant must not address a provider audience");
    }

    // 4. Binding a real child yields no relay handle at all.
    let child = e2e_spawn_child(&mut harness);
    let bound = e2e_runtime()
        .block_on(bind(
            json!({"session_id": session_id, "handle": handle, "pid": child}),
            &e2e_client(),
        ))
        .expect("bind a developer App");
    assert_eq!(bound["bound"], json!(true));
    assert_eq!(
        bound["relay_handle"],
        Value::Null,
        "a developer package must not be handed a relay grant"
    );

    // 5. The live session grant addresses no provider route, and still
    //    carries only the clamped set.
    for audience in [
        authority::Audience::SystemService,
        authority::Audience::Credential,
    ] {
        authority::authority()
            .resolve_session(
                &session_id,
                &authority::Presentation {
                    uid: E2E_UID,
                    pid: child,
                    start_time_ticks: crate::proc::read_start_time_ticks_pub(child),
                    audience,
                    route: "test",
                    session_id: Some(session_id.clone()),
                },
            )
            .expect_err("a developer session grant reaches no provider route");
    }

    crate::provenance::reload_trust();
    match previous_apps {
        Some(value) => std::env::set_var("COS_APPS_DIR", value),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn a_dev_trusted_app_cannot_widen_itself_through_a_session_tool() {
    if !e2e_is_root() {
        eprintln!("skipped: the routed capability partition can only be prepared as root");
        return;
    }
    let mut harness = transient_harness();
    let previous_apps = std::env::var_os("COS_APPS_DIR");
    // Same package, plus a session tool that asks for the world.
    let manifest = E2E_DEV_MANIFEST.replace(
        "  }\n}",
        r#"  },
  "session": {
    "tools": [
      {
        "name": "escalate",
        "summary": "widen after launch",
        "args": [],
        "needs": [
          {"verb": "sys.package",
           "scope": {"kind": "fixed", "scope": {"kind": "name", "value": "nano"}},
           "why": "install"},
          {"verb": "data.kv.read",
           "scope": {"kind": "fixed", "scope": {"kind": "name", "value": "scratch"}},
           "why": "state"}
        ]
      }
    ]
  }
}"#,
    );
    let (root, _apps) = e2e_install_dev_app(&manifest);
    e2e_install_parent_session("cli-parent-tool");

    let result = e2e_runtime()
        .block_on(register(
            json!({"app_id": "scratch", "kind": "operation", "operation": "run", "args": []}),
            &e2e_client(),
        ))
        .expect("register");
    let session_id = result["session_id"].as_str().unwrap().to_string();
    let handle = result["handle"].as_str().unwrap().to_string();
    let child = e2e_spawn_child(&mut harness);
    e2e_runtime()
        .block_on(bind(
            json!({"session_id": session_id, "handle": handle, "pid": child}),
            &e2e_client(),
        ))
        .expect("bind");

    e2e_runtime()
        .block_on(set_transient(
            json!({
                "session_id": session_id,
                "handle": handle,
                "call": {"tool": "escalate"},
            }),
            &e2e_client(),
        ))
        .expect("a benign half of the call is still granted");

    let transient = e2e_read_transient(&session_id).expect("transient set");
    assert_within_developer_ceiling(&transient);
    assert!(
        transient.covers(&Cap::new(Verb::DATA_KV_READ, Scope::name("scratch"))),
        "the allowed half of the tool call must survive"
    );
    assert!(
        !transient.iter().any(|cap| cap.verb == Verb::SYS_PACKAGE),
        "a session tool must not lift a developer package"
    );

    crate::provenance::reload_trust();
    match previous_apps {
        Some(value) => std::env::set_var("COS_APPS_DIR", value),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn the_daemon_resolves_a_dev_trusted_package_to_the_developer_ceiling() {
    // Everything up to the routed registry, without needing root: the
    // package really is verified from disk, its tier really is read
    // from the trust store, and the plan really is clamped by the
    // ceiling the daemon derived — not one a caller supplied.
    let _lock = crate::caps::test_env_lock::env_lock();
    let previous_apps = std::env::var_os("COS_APPS_DIR");
    let (root, _apps) = e2e_install_dev_app(E2E_DEV_MANIFEST);

    let app = installed_app("scratch").expect("a dev-trusted App is installed");
    let ceiling = app_ceiling(&app).expect("ceiling");
    assert!(ceiling.is_developer());
    assert_eq!(ceiling.own_id(), Some("scratch"));
    assert_eq!(app.trust_label(), "developer");

    let delegation = omnipotent_delegation();
    let plan = operation_plan(&app, "run", &[], &delegation, &ceiling).expect("plan");
    let granted = authorize_plan(&delegation, plan, &ceiling, "scratch").expect("authorize");
    assert_within_developer_ceiling(&granted);
    assert!(granted.covers(&Cap::new(Verb::DATA_KV_WRITE, Scope::name("scratch"))));
    assert!(granted.covers(&Cap::new(Verb::FS_READ, Scope::path("/root/scratch/**"))));
    assert!(granted.iter().any(|cap| cap.verb == Verb::UI_NOTIFY));

    // The launch grant built from it addresses only `AppLaunch`.
    let audiences = permitted_audiences(
        Some(&ceiling),
        &[
            authority::Audience::AppLaunch,
            authority::Audience::SystemService,
            authority::Audience::Credential,
            authority::Audience::AppRelay,
        ],
    );
    assert!(audiences.contains(authority::Audience::AppLaunch));
    assert!(!audiences.contains(authority::Audience::AppRelay));
    assert!(!ceiling.allows_relay());
    assert!(!ceiling.allows_granted_path_mounts());

    // Editing the package invalidates the developer grant, so the next
    // authority derivation fails closed rather than running new bytes.
    std::fs::write(root.join("apps/scratch/main.py"), "print('tampered')\n").expect("tamper");
    crate::provenance::verify::invalidate_cache();
    let error = installed_app("scratch").expect_err("a tampered dev package is refused");
    assert!(error.contains("scratch"), "unexpected: {error}");

    crate::provenance::reload_trust();
    match previous_apps {
        Some(value) => std::env::set_var("COS_APPS_DIR", value),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn a_signed_package_keeps_its_privileged_audiences_end_to_end() {
    let _lock = crate::caps::test_env_lock::env_lock();
    let previous_apps = std::env::var_os("COS_APPS_DIR");
    let root = crate::test_env::secure_scratch_dir("clawd-signed-app");
    let apps = root.join("apps");
    let dir = apps.join("scratch");
    std::fs::create_dir_all(&dir).expect("app dir");
    std::fs::write(dir.join("app.json"), E2E_DEV_MANIFEST).expect("manifest");
    std::fs::write(dir.join("main.py"), "print('hi')\n").expect("entrypoint");
    crate::test_env::sign_test_package(&dir, crate::provenance::PackageKind::App, "scratch");
    std::env::set_var("COS_APPS_DIR", &apps);

    let app = installed_app("scratch").expect("a signed App is installed");
    let ceiling = app_ceiling(&app).expect("ceiling");
    assert!(!ceiling.is_developer());
    assert!(ceiling.allows_relay());

    let delegation = omnipotent_delegation();
    let plan = operation_plan(&app, "run", &[], &delegation, &ceiling).expect("plan");
    let granted = authorize_plan(&delegation, plan, &ceiling, "scratch").expect("authorize");
    for (verb, name) in FORBIDDEN_FOR_DEVELOPER {
        assert!(
            granted.iter().any(|cap| cap.verb == *verb),
            "`{name}` must survive for signed content"
        );
    }

    crate::provenance::reload_trust();
    match previous_apps {
        Some(value) => std::env::set_var("COS_APPS_DIR", value),
        None => std::env::remove_var("COS_APPS_DIR"),
    }
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// Relaying for a revoked package
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn a_relay_refuses_a_session_whose_package_was_revoked() {
    // The relay is the one route a sandboxed worker uses to reach a
    // privileged provider, and the session grant behind it lives for
    // `SESSION_GRANT_TTL`. Waiting for that to expire would leave
    // revoked code driving `system.*` routes for minutes, so the gate
    // runs per call.
    let _lock = crate::caps::test_env_lock::env_lock();
    let scratch = crate::test_env::secure_scratch_dir("relay-revoked");
    let _proc = crate::test_env::TestEnvVarGuard::set("COS_PROVENANCE_RUNTIME_DIR", &scratch);

    let session_id = "app-relay-revoked";
    let (pid, ticks) = this_process();
    let client = ClientIdentity {
        pid: Some(pid),
        uid: Some(crate::provenance::fsec::effective_uid()),
        gid: Some(0),
        execution_uid: None,
        start_time_ticks: ticks,
        attended_local: false,
    };

    // A relay grant names an App session, so the record *should* exist.
    // Its absence means the one thing that could confirm the package is
    // still trusted is gone, and that fails closed rather than relaying.
    let missing = e2e_runtime()
        .block_on(assert_relay_package_live(&client, session_id))
        .expect_err("a relay with no running-instance record must fail closed");
    assert!(
        missing.contains("no longer trusted") && missing.contains("no running-instance record"),
        "unexpected: {missing}"
    );

    // A recorded, un-revoked instance relays normally.
    let owner = crate::provenance::fsec::effective_uid();
    crate::provenance::runtime::register_operator_mcp(owner, session_id);
    e2e_runtime()
        .block_on(assert_relay_package_live(&client, session_id))
        .expect("a recorded instance relays");

    // Once it is marked, the very next relay is refused — no grant TTL,
    // no notification, no restart.
    crate::provenance::runtime::mark_for_shutdown(
        crate::provenance::fsec::effective_uid(),
        session_id,
        "publisher key was revoked",
    );
    let error = e2e_runtime()
        .block_on(assert_relay_package_live(&client, session_id))
        .expect_err("a revoked session must not relay");
    assert!(
        error.contains("no longer trusted") && error.contains("publisher key was revoked"),
        "unexpected: {error}"
    );

    crate::provenance::runtime::deregister(crate::provenance::fsec::effective_uid(), session_id);
    let _ = std::fs::remove_dir_all(&scratch);
}
