use super::*;

use crate::approvals::GrantDuration;
use crate::caps::{Cap, CapSet, Role, Scope, Verb};
use crate::proc::SessionInfo;
use serde_json::json;

const OWNER_UID: u32 = 4242;

fn home() -> std::path::PathBuf {
    std::path::PathBuf::from("/home/u")
}

fn args(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|token| (*token).to_string()).collect()
}

fn parse(subsystem: &str, command: &str, tokens: &[&str]) -> Result<SchedulerCommand, String> {
    SchedulerCommand::parse(&json!({
        "subsystem": subsystem,
        "command": command,
        "args": args(tokens),
    }))
}

fn request(subsystem: &str, command: &str, tokens: &[&str]) -> SchedulerCommand {
    parse(subsystem, command, tokens).expect("valid scheduler request")
}

fn this_process() -> (u32, u64) {
    let pid = std::process::id();
    (
        pid,
        crate::proc::read_start_time_ticks_pub(pid).unwrap_or(1),
    )
}

fn session_row(session_id: &str, pid: u32, app_id: Option<&str>, caps: CapSet) -> SessionInfo {
    SessionInfo {
        session_id: session_id.to_string(),
        pid,
        command: vec!["test".to_string()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("test".to_string()),
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

/// Authority of a peer `clawd` could not tie to any session it
/// registered — the ordinary desktop CLI. Built exactly the way
/// `caller_authority` builds it for an unregistered peer;
/// `an_unconfined_terminal_process_holds_no_scheduler_authority` pins
/// the two together against a real process.
fn unregistered_authority(uid: u32, pid: u32, start: u64) -> CallerAuthority {
    CallerAuthority {
        uid,
        parent: None,
        grant_session: unregistered_grant_identity(uid, pid, start),
        requester: format!("uid:{uid} pid:{pid} start:{start}"),
        delegable: CapSet::new(),
        ceiling: scheduled_ceiling(&home())
            .intersect(&crate::clawd::system_caps::local_launcher_ceiling(&home())),
        tier: Some(Role::Worker.credential_tier()),
        role: Some(Role::Worker.name().to_string()),
    }
}

/// Redirect the approvals store into a throwaway directory so tests
/// that exercise a denial or a grant never touch real state. Holds the
/// shared env lock for the duration.
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

/// Approval request ids a denial reported, as the caller sees them.
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

fn approve_pending(uid: u32) {
    for request in crate::approvals::list_pending_for_owner(Some(uid)) {
        crate::approvals::approve_for_owner(
            &request.id,
            GrantDuration::Once,
            Some("uid:0".to_string()),
            None,
            Some(uid),
        )
        .expect("approve");
    }
}

// ---------------------------------------------------------------------------
// Request validation
// ---------------------------------------------------------------------------

#[test]
fn rejects_unknown_subsystems_and_commands() {
    let error = parse("systemd", "list", &[]).expect_err("unknown subsystem");
    assert!(error.contains("unsupported scheduler subsystem"), "{error}");

    let error = parse("cron", "exec", &["job"]).expect_err("unknown cron command");
    assert!(error.contains("unsupported cron command"), "{error}");

    let error = parse("triggers", "logs", &["rule"]).expect_err("unknown trigger command");
    assert!(error.contains("unsupported triggers command"), "{error}");
}

#[test]
fn tick_stays_reserved_for_the_kernel_heartbeat() {
    for subsystem in ["cron", "triggers"] {
        let error = parse(subsystem, "tick", &[]).expect_err("tick is not brokered");
        assert!(
            error.contains("reserved for the kernel heartbeat"),
            "{error}"
        );
    }
}

#[test]
fn rejects_malformed_resource_identifiers_before_authorization() {
    for id in ["../../etc/cos", "job id", "job/../other", ""] {
        let error = parse("cron", "run", &[id]).expect_err("cron id must be validated");
        assert!(error.contains("job id"), "{error}");
    }
    let error = parse("cron", "status", &[]).expect_err("a job id is required");
    assert!(error.contains("requires a job id"), "{error}");

    for id in [".hidden", "rule/../other", "rule id"] {
        let error = parse("triggers", "run", &[id]).expect_err("trigger id must be validated");
        assert!(error.contains("trigger id"), "{error}");
    }
    let error = parse("triggers", "add", &["--prompt", "hi"]).expect_err("a rule id is required");
    assert!(error.contains("requires a rule id"), "{error}");

    assert_eq!(
        request("cron", "run", &["nightly-backup"])
            .target
            .as_deref(),
        Some("nightly-backup")
    );
    assert_eq!(
        request("triggers", "add", &["--id", "on.load", "--prompt", "hi"])
            .target
            .as_deref(),
        Some("on.load")
    );
}

#[test]
fn rejects_arguments_that_are_not_bounded_strings() {
    let error = SchedulerCommand::parse(&json!({
        "subsystem": "cron",
        "command": "list",
        "args": [{"job": "x"}],
    }))
    .expect_err("args must be strings");
    assert!(error.contains("invalid scheduler args"), "{error}");

    let many: Vec<String> = (0..MAX_ARGS + 1).map(|index| index.to_string()).collect();
    let error = SchedulerCommand::parse(&json!({
        "subsystem": "cron",
        "command": "list",
        "args": many,
    }))
    .expect_err("argument count is bounded");
    assert!(error.contains("at most"), "{error}");

    let error = parse("cron", "run", &["job\0name"]).expect_err("NUL is rejected");
    assert!(error.contains("NUL") || error.contains("job id"), "{error}");
}

#[test]
fn credential_injection_is_bound_to_the_named_credentials() {
    let job = request(
        "cron",
        "add",
        &[
            "sync",
            "--schedule",
            "* * * * *",
            "--command",
            "true",
            "--credentials",
            "aws, aws ,github",
        ],
    );
    assert_eq!(
        job.credentials,
        vec!["aws".to_string(), "github".to_string()]
    );
    let delegated = job.delegated_caps();
    assert!(delegated.contains(&Cap::new(Verb::PROC_SPAWN, Scope::Wild)));
    assert!(delegated.contains(&Cap::new(Verb::SECRET_READ, Scope::name("default/aws"))));
    assert!(delegated.contains(&Cap::new(Verb::SECRET_READ, Scope::name("default/github"))));
    assert!(
        !delegated.contains(&Cap::new(Verb::SECRET_READ, Scope::Wild)),
        "credential authority must never be widened to every secret"
    );

    let error = parse(
        "cron",
        "add",
        &["sync", "--credentials", "openai/../../root-token"],
    )
    .expect_err("credential names are validated");
    assert!(error.contains("invalid credential name"), "{error}");
}

// ---------------------------------------------------------------------------
// Peer authority
// ---------------------------------------------------------------------------

/// The old gate accepted any process with `NoNewPrivs == 0` and a
/// controlling terminal — which is every ordinary desktop process,
/// including one that allocated a pty for itself. Neither fact is
/// consulted any more, so this test process (unconfined, frequently
/// on a tty) still holds nothing it can delegate.
#[cfg(target_os = "linux")]
#[test]
fn an_unconfined_terminal_process_holds_no_scheduler_authority() {
    let (pid, start) = this_process();
    let derived = caller_authority(&[], OWNER_UID, pid, start, &home())
        .expect("an unregistered peer resolves to daemon policy");
    let expected = unregistered_authority(OWNER_UID, pid, start);

    assert!(derived.parent.is_none());
    assert!(
        derived.delegable.is_empty(),
        "an unregistered peer may delegate nothing"
    );
    assert_eq!(derived.ceiling, expected.ceiling);
    assert_eq!(derived.grant_session, expected.grant_session);
    assert_eq!(derived.role.as_deref(), Some(Role::Worker.name()));
    assert_eq!(derived.tier, Some(Role::Worker.credential_tier()));
    assert!(derived.grant_session.contains(&format!("pid={pid}")));
    assert!(derived.grant_session.contains(&format!("start={start}")));
}

#[test]
fn registered_session_authority_is_attenuated_to_the_scheduled_ceiling() {
    let (pid, _) = this_process();
    let held =
        Role::Admin.caps_with_scopes(Some(Scope::Wild), Some(Scope::Wild), Some(Scope::Wild));
    let sessions = vec![session_row("cron-parent", pid, None, held)];
    let authority =
        caller_authority(&sessions, OWNER_UID, pid, 7, &home()).expect("registered authority");

    assert_eq!(authority.parent.as_deref(), Some("cron-parent"));
    assert_eq!(authority.grant_session, "cron-parent");
    assert!(authority
        .delegable
        .covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild)));
    assert!(
        !authority
            .delegable
            .covers(&Cap::new(Verb::SYS_CONFIG, Scope::Wild)),
        "an admin session must not delegate machine-wide authority into a job"
    );
    assert!(
        !authority
            .delegable
            .covers(&Cap::new(Verb::FS_WRITE, Scope::path("/etc/shadow"))),
        "delegated filesystem authority stays inside the owner home"
    );
    assert!(authority
        .delegable
        .covers(&Cap::new(Verb::FS_WRITE, Scope::path("/home/u/notes.txt"))));
}

#[test]
fn an_app_session_cannot_manage_proactive_jobs() {
    let (pid, _) = this_process();
    let caps =
        Role::AgentHost.caps_with_scopes(Some(Scope::Wild), Some(Scope::Wild), Some(Scope::Wild));
    let sessions = vec![session_row("app-1", pid, Some("fs"), caps)];
    let error =
        caller_authority(&sessions, OWNER_UID, pid, 7, &home()).expect_err("App sessions denied");
    assert!(error.contains("cannot manage proactive jobs"), "{error}");
}

#[test]
fn a_session_bound_to_another_process_is_not_this_peer() {
    let (pid, _) = this_process();
    let caps =
        Role::AgentHost.caps_with_scopes(Some(Scope::Wild), Some(Scope::Wild), Some(Scope::Wild));
    // Same row, but recorded against a start time this pid never had.
    let mut stale = session_row("cron-parent", pid, None, caps);
    stale.start_time_ticks = Some(u64::MAX);

    #[cfg(target_os = "linux")]
    {
        let authority = caller_authority(&[stale], OWNER_UID, pid, 7, &home())
            .expect("a recycled pid falls back to unregistered");
        assert!(authority.parent.is_none());
        assert!(authority.delegable.is_empty());
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stale;
    }
}

#[cfg(target_os = "linux")]
#[test]
fn an_unresolvable_ancestry_fails_closed() {
    let error = caller_authority(&[], OWNER_UID, u32::MAX, 7, &home())
        .expect_err("an ancestry clawd cannot walk must not fall back to policy");
    assert!(error.contains("ancestry"), "{error}");
}

#[cfg(target_os = "linux")]
#[test]
fn a_root_peer_is_authorized_from_its_own_authority() {
    let _sandbox = approval_sandbox();
    let (pid, start) = this_process();
    let authority = caller_authority(&[], 0, pid, start, &std::path::PathBuf::from("/root"))
        .expect("root authority");
    assert_eq!(authority.tier, Some(Role::Admin.credential_tier()));

    let caps = authorize(&request("cron", "add", &["nightly"]), &authority)
        .expect("root needs no extra decision");
    assert!(caps.covers(&Cap::new(Verb::TIME_CRON, Scope::Wild)));
    assert!(caps.covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild)));
    assert!(
        crate::approvals::list_pending_for_owner(None).is_empty(),
        "root must not queue an approval for itself"
    );
}

// ---------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------

#[test]
fn owner_scoped_commands_get_only_the_capability_their_gate_needs() {
    let _sandbox = approval_sandbox();
    let authority = CallerAuthority {
        uid: OWNER_UID,
        parent: None,
        grant_session: unregistered_grant_identity(OWNER_UID, 99, 7),
        requester: "uid:4242 pid:99 start:7".to_string(),
        delegable: CapSet::new(),
        ceiling: scheduled_ceiling(&home()),
        tier: Some(Role::Worker.credential_tier()),
        role: Some(Role::Worker.name().to_string()),
    };

    let caps = authorize(&request("cron", "list", &[]), &authority).expect("listing is owned work");
    assert_eq!(
        caps,
        CapSet::from_caps([Cap::new(Verb::TIME_CRON, Scope::Wild)])
    );

    let caps = authorize(&request("cron", "logs", &["nightly"]), &authority).expect("own logs");
    assert_eq!(
        caps,
        CapSet::from_caps([Cap::new(Verb::DATA_LOG_READ, Scope::Wild)])
    );

    let caps =
        authorize(&request("cron", "remove", &["nightly"]), &authority).expect("retire own job");
    assert_eq!(
        caps,
        CapSet::from_caps([Cap::new(Verb::TIME_CRON, Scope::Wild)])
    );
    assert!(
        crate::approvals::list_pending_for_owner(Some(OWNER_UID)).is_empty(),
        "owner-scoped work must not queue approvals"
    );
}

#[test]
fn creating_a_job_without_provable_authority_is_denied_and_files_one_decision() {
    let _sandbox = approval_sandbox();
    let authority = unregistered_authority(OWNER_UID, 99, 7);
    let job = request("cron", "add", &["nightly", "--command", "true"]);

    let error = authorize(&job, &authority).expect_err("no authority to delegate");
    assert_eq!(approval_requests(&error).len(), 1);
    let pending = crate::approvals::list_pending_for_owner(Some(OWNER_UID));
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].verb, Verb::PROC_SPAWN.as_str());
    assert_eq!(pending[0].session, authority.grant_session);

    authorize(&job, &authority).expect_err("still denied while the decision is pending");
    assert_eq!(
        crate::approvals::list_pending_for_owner(Some(OWNER_UID)).len(),
        1,
        "a retry must not queue a second prompt for one decision"
    );
}

#[test]
fn an_approved_decision_authorizes_one_operation_and_is_then_spent() {
    let _sandbox = approval_sandbox();
    let authority = unregistered_authority(OWNER_UID, 99, 7);
    let job = request("cron", "add", &["nightly", "--command", "true"]);

    authorize(&job, &authority).expect_err("denied first");
    approve_pending(OWNER_UID);

    let caps = authorize(&job, &authority).expect("approved once");
    assert!(caps.covers(&Cap::new(Verb::TIME_CRON, Scope::Wild)));
    assert!(caps.covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild)));

    authorize(&job, &authority).expect_err("the grant does not survive the operation");
}

#[test]
fn a_manual_run_needs_its_own_one_shot_decision() {
    let _sandbox = approval_sandbox();
    let authority = unregistered_authority(OWNER_UID, 99, 7);
    let manual = request("cron", "run", &["nightly"]);

    let error = authorize(&manual, &authority).expect_err("running a job spends authority");
    assert_eq!(approval_requests(&error).len(), 1);
    approve_pending(OWNER_UID);

    let caps = authorize(&manual, &authority).expect("approved run");
    assert_eq!(
        caps,
        CapSet::from_caps([Cap::new(Verb::PROC_SPAWN, Scope::Wild)])
    );
    assert!(
        !caps.covers(&Cap::new(Verb::TIME_CRON, Scope::Wild)),
        "a manual run gets the execution verb only"
    );
    authorize(&manual, &authority).expect_err("the run decision is not reusable");
}

#[test]
fn a_decision_approved_for_another_peer_or_owner_is_not_accepted() {
    let _sandbox = approval_sandbox();
    let authority = unregistered_authority(OWNER_UID, 99, 7);
    let job = request("triggers", "add", &["--id", "watcher", "--prompt", "hi"]);

    authorize(&job, &authority).expect_err("denied first");
    approve_pending(OWNER_UID);

    // Same user, different process: the identity clawd derives from
    // the peer changes, so a sibling cannot spend the decision.
    let sibling = unregistered_authority(OWNER_UID, 100, 7);
    authorize(&job, &sibling).expect_err("a sibling process cannot spend another's decision");

    // Same process facts, different uid: owner filtering still applies.
    let other_user = unregistered_authority(OWNER_UID + 1, 99, 7);
    authorize(&job, &other_user).expect_err("another user cannot spend this decision");

    authorize(&job, &authority).expect("the peer the decision was made for still proceeds");
}

#[test]
fn a_registered_session_needs_no_new_decision() {
    let _sandbox = approval_sandbox();
    let (pid, _) = this_process();
    let held =
        Role::AgentHost.caps_with_scopes(Some(Scope::Wild), Some(Scope::Wild), Some(Scope::Wild));
    let sessions = vec![session_row("cron-parent", pid, None, held)];
    let authority =
        caller_authority(&sessions, OWNER_UID, pid, 7, &home()).expect("registered authority");

    let caps = authorize(&request("cron", "add", &["nightly"]), &authority)
        .expect("an authenticated session delegates its own authority");
    assert!(caps.covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild)));
    assert!(
        crate::approvals::list_pending_for_owner(Some(OWNER_UID)).is_empty(),
        "authenticated authority must not queue approvals"
    );
}

#[test]
fn an_observer_session_cannot_delegate_execution_authority() {
    let _sandbox = approval_sandbox();
    let (pid, _) = this_process();
    let held = Role::Observer.caps_with_scopes(
        Some(Scope::path("/home/u/**")),
        Some(Scope::Wild),
        Some(Scope::Wild),
    );
    let sessions = vec![session_row("observer", pid, None, held)];
    let authority =
        caller_authority(&sessions, OWNER_UID, pid, 7, &home()).expect("registered authority");

    let error = authorize(&request("cron", "add", &["nightly"]), &authority)
        .expect_err("an observer holds no proc.spawn to delegate");
    assert_eq!(approval_requests(&error).len(), 1);
}

#[test]
fn an_approved_job_snapshot_never_reaches_beyond_the_owner() {
    let _sandbox = approval_sandbox();
    let authority = unregistered_authority(OWNER_UID, 99, 7);
    let job = request(
        "cron",
        "add",
        &["nightly", "--command", "true", "--credentials", "aws"],
    );

    authorize(&job, &authority).expect_err("denied first");
    approve_pending(OWNER_UID);
    let caps = authorize(&job, &authority).expect("approved");

    assert!(caps.covers(&Cap::new(Verb::SECRET_READ, Scope::name("default/aws"))));
    for denied in [
        Cap::new(Verb::SECRET_READ, Scope::name("default/github")),
        Cap::new(Verb::SECRET_READ, Scope::Wild),
        Cap::new(Verb::SYS_CONFIG, Scope::Wild),
        Cap::new(Verb::SYS_PACKAGE, Scope::Wild),
        Cap::new(Verb::NET_MANAGE, Scope::Wild),
        Cap::new(Verb::DATA_BACKUP, Scope::Wild),
        Cap::new(Verb::FS_READ, Scope::path("/etc/shadow")),
        Cap::new(Verb::FS_WRITE, Scope::path("/**")),
    ] {
        assert!(
            !caps.covers(&denied),
            "{}:{} must never reach a scheduled job",
            denied.verb.as_str(),
            denied.scope
        );
    }
}

// ---------------------------------------------------------------------------
// Session shape
// ---------------------------------------------------------------------------

#[test]
fn the_trusted_session_carries_only_the_authorized_operation() {
    let _sandbox = approval_sandbox();
    let authority = unregistered_authority(OWNER_UID, 99, 7);
    let listing = request("cron", "list", &[]);
    let caps = authorize(&listing, &authority).expect("owned work");
    let session = trusted_session(&listing, &authority, caps, &home());

    assert_eq!(session.pid, std::process::id());
    assert!(session.app_id.is_none());
    assert!(!session.pending_bind);
    assert_eq!(session.command, vec!["cron.list".to_string()]);
    assert_eq!(session.role.as_deref(), Some(Role::Worker.name()));
    assert_eq!(session.tier, Some(Role::Worker.credential_tier()));
    assert_eq!(
        session.caps,
        Some(CapSet::from_caps([Cap::new(Verb::TIME_CRON, Scope::Wild)])),
        "a listing must not install agent-host authority"
    );
}

#[tokio::test]
async fn the_trusted_session_does_not_survive_the_operation() {
    let _sandbox = approval_sandbox();
    let authority = unregistered_authority(OWNER_UID, 99, 7);
    let listing = request("cron", "list", &[]);
    let caps = authorize(&listing, &authority).expect("owned work");
    let session = trusted_session(&listing, &authority, caps, &home());
    let session_id = session.session_id.clone();

    let seen = crate::proc::with_trusted_session_override(session, async {
        crate::proc::current_trusted_session_for_caps().map(|session| session.session_id)
    })
    .await;
    assert_eq!(seen.as_deref(), Some(session_id.as_str()));
    assert!(
        crate::proc::current_trusted_session_for_caps().is_none(),
        "the override must not outlive the operation it authorized"
    );
}

/// Nothing in the request may become authority: the daemon reads the
/// subsystem, command and arguments and derives everything else.
#[test]
fn request_fields_cannot_widen_authority() {
    let _sandbox = approval_sandbox();
    let claimed = SchedulerCommand::parse(&json!({
        "subsystem": "cron",
        "command": "add",
        "args": ["nightly"],
        "session": "scheduler-client-forged",
        "owner_uid": 0,
        "uid": 0,
        "role": "admin",
        "tier": 0,
        "caps": [{"verb": "sys.config", "scope": {"kind": "wild"}}],
        "parent_caps": [{"verb": "sys.config", "scope": {"kind": "wild"}}],
    }))
    .expect("extra fields are ignored, not trusted");
    assert_eq!(claimed.subsystem, Subsystem::Cron);
    assert_eq!(claimed.command, "add");
    assert_eq!(claimed.target.as_deref(), Some("nightly"));

    let authority = unregistered_authority(OWNER_UID, 99, 7);
    let error = authorize(&claimed, &authority).expect_err("claimed authority is not authority");
    assert_eq!(approval_requests(&error).len(), 1);
    approve_pending(OWNER_UID);
    let caps = authorize(&claimed, &authority).expect("approved");
    assert!(!caps.covers(&Cap::new(Verb::SYS_CONFIG, Scope::Wild)));

    let session = trusted_session(&claimed, &authority, caps, &home());
    assert_ne!(session.session_id, "scheduler-client-forged");
    assert_eq!(session.tier, Some(Role::Worker.credential_tier()));
    assert_eq!(session.role.as_deref(), Some(Role::Worker.name()));
}
