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

fn app_from(manifest: &str) -> App {
    let manifest = Manifest::from_json(manifest).expect("test manifest");
    let dir = std::path::PathBuf::from("/usr/lib/cos/apps").join(&manifest.id);
    App { manifest, dir }
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

/// Derive and settle a plan the way egister does, so tests exercise
/// the real authorization path.
fn operation_caps(
    app: &App,
    operation: &str,
    args: &[String],
    delegation: &Delegation,
) -> Result<CapSet, BrokerError> {
    let plan = operation_plan(app, operation, args, delegation)?;
    authorize_plan(delegation, plan)
}

fn gui_caps(app: &App, exec: &str, delegation: &Delegation) -> Result<CapSet, BrokerError> {
    let plan = gui_plan(app, exec, delegation)?;
    authorize_plan(delegation, plan)
}

fn with_invoke_cap(
    caps: CapSet,
    app_id: &str,
    delegation: &Delegation,
) -> Result<CapSet, BrokerError> {
    let mut plan = LaunchPlan::default();
    plan.inherit(caps.iter().cloned());
    plan.require(Cap::new(Verb::AGENT_INVOKE, Scope::name(app_id)), delegation);
    authorize_plan(delegation, plan)
}

/// Launcher authority for a synthetic peer process.
fn authority_for(pid: u32, start_time_ticks: Option<u64>, parent: Option<&str>) -> LauncherAuthority {
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
    Delegation::new(&authority_for(pid, Some(ticks), None), 1000, &home(), &serde_json::json!({}))
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
    let data = error.data.as_ref().expect("a denial must report its requests");
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
    assert!(error.message.contains("sys.identity"), "unexpected: {error}");

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
    assert!(error.message.contains("sys.identity"), "unexpected: {error}");
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
    assert!(error.message.contains("sys.identity"), "unexpected: {error}");

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
        (Verb::SYS_CONFIG, Scope::name("accounts")),
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
    assert!(error.message.contains("sys.identity"), "unexpected: {error}");
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
    assert!(caps.covers(&Cap::new(Verb::SYS_CONFIG, Scope::path("/etc/cos/agent.toml"))));
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
    assert!(denial.message.contains("/etc/passwd"), "unexpected: {denial}");
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

        let denial = operation_caps(&app, "create-user", &args(&["alice"]), &launcher)
            .expect_err("denied");
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
    let error = launcher_authority(&rows, pid, ticks, &home())
        .expect_err("App launchers are rejected");
    assert!(error.contains("app-1"), "unexpected error: {error}");
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
        let inherited = inherited_wild_caps(verb, &delegation).expect("inheritable");
        for cap in &inherited {
            assert!(delegation.ceiling.covers(cap));
            assert!(!matches!(cap.scope, Scope::Wild));
        }
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
    let caps = gui_caps(&test_app(), "--gui", &delegation).expect("gui caps");
    assert!(
        caps.is_empty(),
        "an unbounded ceiling must not satisfy a GUI wildcard need"
    );
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
// Launch handles
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

#[test]
fn handle_is_bound_to_its_session_launcher_and_owner() {
    let authority = test_authority();
    let token = issue_handle("app-handle-1", 4242, &authority);

    assert!(authorize_handle(&token, "app-handle-1", 4242, authority.pid, false).is_ok());
    assert!(authorize_handle(&token, "app-other", 4242, authority.pid, false).is_err());
    assert!(authorize_handle(&token, "app-handle-1", 4343, authority.pid, false).is_err());
    assert!(authorize_handle(&token, "app-handle-1", 4242, authority.pid + 1, false).is_err());
    assert!(authorize_handle("not-a-handle", "app-handle-1", 4242, authority.pid, false).is_err());

    release_handle(&token);
    assert!(authorize_handle(&token, "app-handle-1", 4242, authority.pid, false).is_err());
}

#[test]
fn handle_binds_a_process_only_once() {
    let authority = test_authority();
    let token = issue_handle("app-handle-2", 4242, &authority);

    assert!(authorize_handle(&token, "app-handle-2", 4242, authority.pid, true).is_ok());
    mark_handle_bound(&token);
    assert!(
        authorize_handle(&token, "app-handle-2", 4242, authority.pid, true).is_err(),
        "the bind grant must be single use"
    );
    assert!(
        authorize_handle(&token, "app-handle-2", 4242, authority.pid, false).is_ok(),
        "control operations remain available to the same launcher"
    );
    release_handle(&token);
}

#[test]
fn expired_unbound_handle_is_pruned() {
    let authority = test_authority();
    let token = issue_handle("app-handle-3", 4242, &authority);
    {
        let mut store = handles();
        let handle = store.get_mut(&token).expect("issued handle");
        handle.bind_deadline = Instant::now() - Duration::from_secs(1);
    }
    let error = authorize_handle(&token, "app-handle-3", 4242, authority.pid, false)
        .expect_err("expired handle");
    assert!(error.contains("unknown or expired"), "unexpected: {error}");
}

#[test]
fn handle_for_an_exited_launcher_is_refused() {
    let mut authority = test_authority();
    // A pid/start-time pair that cannot be alive: pid 0 is never a
    // real process for `kill(0)` aliveness purposes.
    authority.pid = 0;
    authority.start_time_ticks = Some(1);
    let token = issue_handle("app-handle-4", 4242, &authority);
    assert!(authorize_handle(&token, "app-handle-4", 4242, 0, false).is_err());
    release_handle(&token);
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
    for command in [
        "app_session.register",
        "app_session.register_native",
        "mcp_session.register",
        "app_session.bind",
        "app_session.set_transient",
        "app_session.deregister",
    ] {
        assert!(owns_command(command), "{command} must be routed here");
    }
    assert!(!owns_command("system.package.control"));
    assert!(!owns_command("task.submit"));
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
