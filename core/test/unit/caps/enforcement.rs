use super::*;
use std::io::Write;

/// Test guard that sets `COS_DATA_DIR` to a fresh tmp dir, writes a
/// registry JSON, sets `COS_SESSION` + `COS_PERMS_MODE`, and
/// restores the previous env on drop.
///
/// Because Rust runs unit tests in parallel by default and env vars
/// are process-global, callers must serialise via the EnvLock
/// mutex below.
struct EnvGuard {
    prev_data_dir: Option<String>,
    prev_log_dir: Option<String>,
    prev_session: Option<String>,
    prev_mode: Option<String>,
    _tmp: tempfile::TempDir,
}

impl EnvGuard {
    fn new(registry_json: &str, session: Option<&str>, mode: Option<&str>) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let proc_dir = tmp.path().join("proc");
        std::fs::create_dir_all(&proc_dir).unwrap();
        let mut f = std::fs::File::create(proc_dir.join("registry.json")).unwrap();
        f.write_all(registry_json.as_bytes()).unwrap();

        let prev_data_dir = std::env::var("COS_DATA_DIR").ok();
        let prev_log_dir = std::env::var("COS_LOG_DIR").ok();
        let prev_session = std::env::var("COS_SESSION").ok();
        let prev_mode = std::env::var("COS_PERMS_MODE").ok();

        std::env::set_var("COS_DATA_DIR", tmp.path());
        // Redirect caps.jsonl writes into the test tmpdir so the
        // audit hook doesn't litter the host's logs dir.
        std::env::set_var("COS_LOG_DIR", tmp.path());
        match session {
            Some(s) => std::env::set_var("COS_SESSION", s),
            None => std::env::remove_var("COS_SESSION"),
        }
        match mode {
            Some(m) => std::env::set_var("COS_PERMS_MODE", m),
            None => std::env::remove_var("COS_PERMS_MODE"),
        }
        Self {
            prev_data_dir,
            prev_log_dir,
            prev_session,
            prev_mode,
            _tmp: tmp,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev_data_dir {
            Some(v) => std::env::set_var("COS_DATA_DIR", v),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
        match &self.prev_log_dir {
            Some(v) => std::env::set_var("COS_LOG_DIR", v),
            None => std::env::remove_var("COS_LOG_DIR"),
        }
        match &self.prev_session {
            Some(v) => std::env::set_var("COS_SESSION", v),
            None => std::env::remove_var("COS_SESSION"),
        }
        match &self.prev_mode {
            Some(v) => std::env::set_var("COS_PERMS_MODE", v),
            None => std::env::remove_var("COS_PERMS_MODE"),
        }
    }
}

// Shared env-var mutex — see `caps::test_env_lock`. Lives in the
// parent module so it's also held by `caps::bootstrap` tests
// (which mutate the same `COS_*` vars).
use crate::caps::test_env_lock::env_lock;

fn registry_with_caps(sid: &str, caps_json: &str) -> String {
    // pid=0 disables the ancestry check (see the require() body).
    format!(
        r#"{{
          "sessions": [
            {{
              "session_id": "{sid}",
              "pid": 0,
              "caps": {caps_json}
            }}
          ]
        }}"#
    )
}

fn with_local_invocation<R>(id: &str, f: impl FnOnce() -> R) -> R {
    crate::approvals::LocalApprovalInvocation::new(id)
        .unwrap()
        .sync_scope(f)
}

#[test]
fn permissive_allows_when_no_session() {
    let _lock = env_lock();
    let _g = EnvGuard::new(r#"{"sessions":[]}"#, None, Some("permissive"));
    assert!(require(Verb::FS_READ, Scope::path("/etc")).is_ok());
}

#[test]
fn strict_denies_when_no_session() {
    let _lock = env_lock();
    let _g = EnvGuard::new(r#"{"sessions":[]}"#, None, Some("strict"));
    let err = require(Verb::FS_READ, Scope::path("/etc")).unwrap_err();
    assert!(matches!(
        err.reason,
        super::super::denial::DenialReason::NoSession
    ));
}

#[test]
fn strict_denies_unknown_session() {
    let _lock = env_lock();
    let _g = EnvGuard::new(r#"{"sessions":[]}"#, Some("missing"), Some("strict"));
    let err = require(Verb::FS_READ, Scope::path("/etc")).unwrap_err();
    assert!(matches!(
        err.reason,
        super::super::denial::DenialReason::NoSession
    ));
}

#[test]
fn allows_when_session_caps_cover_request() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s1", caps);
    let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
    assert!(require(Verb::FS_READ, Scope::path("/home/jay/notes.md")).is_ok());
}

#[test]
fn denies_with_scope_out_of_range_when_verb_held_but_path_outside() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s1", caps);
    let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
    let err = require(Verb::FS_READ, Scope::path("/etc/passwd")).unwrap_err();
    assert!(matches!(
        err.reason,
        super::super::denial::DenialReason::ScopeOutOfRange
    ));
    // The granted_scopes echo back exactly what the session holds.
    assert_eq!(err.granted_scopes.len(), 1);
}

#[test]
fn denies_with_verb_not_granted_when_verb_missing() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s1", caps);
    let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
    with_local_invocation("test:missing-verb", || {
        let err = require(Verb::FS_DELETE, Scope::path("/home/jay/x")).unwrap_err();
        assert!(matches!(
            err.reason,
            super::super::denial::DenialReason::VerbNotGranted
        ));
        assert!(err
            .hint
            .as_deref()
            .is_some_and(|hint| { hint.contains("approval request") && hint.contains("pending") }));
        let pending = crate::approvals::list_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].verb, Verb::FS_DELETE.as_str());
        assert_eq!(pending[0].scope, Scope::path("/home/jay/x"));
        assert_eq!(pending[0].risk, Some(crate::caps::Risk::High));
        assert_eq!(
            pending[0].context,
            Some(crate::caps::ConsentContext::Attended)
        );
    });
}

/// Every capability denial gets an exact, one-shot request — not only
/// the high-risk ones. The system-Agent baseline withholds low- and
/// medium-risk verbs whose *resource* is dangerous, so a risk floor
/// here would leave those denials with no route to consent.
#[test]
fn low_risk_denial_creates_an_exact_approval_request() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s1", caps);
    let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
    with_local_invocation("test:low-risk", || {
        let err = require(Verb::NET_RESOLVE, Scope::host("example.com")).unwrap_err();
        assert!(err
            .hint
            .as_deref()
            .is_some_and(|hint| { hint.contains("approval request") && hint.contains("pending") }));
        let pending = crate::approvals::list_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].verb, Verb::NET_RESOLVE.as_str());
        assert_eq!(pending[0].scope, Scope::host("example.com"));
        assert_eq!(pending[0].risk, Some(crate::caps::Risk::Low));
    });
}

/// A path denial inside a verb the session already holds is the
/// "read exactly this one file" case: it files a request for that
/// resource, and approving it authorises nothing adjacent.
#[test]
fn scope_denial_files_a_request_for_that_exact_resource() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s1", caps);
    let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));

    with_local_invocation("test:scope-denial", || {
        let err = require(Verb::FS_READ, Scope::path("/etc/hosts")).unwrap_err();
        assert!(matches!(
            err.reason,
            super::super::denial::DenialReason::ScopeOutOfRange
        ));
        let pending = crate::approvals::list_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].scope, Scope::path("/etc/hosts"));
        crate::approvals::approve(
            &pending[0].id,
            crate::approvals::GrantDuration::Once,
            None,
            None,
        )
        .unwrap();

        // Spent exactly once, on exactly that path.
        assert!(require(Verb::FS_READ, Scope::path("/etc/hosts")).is_ok());
        assert!(require(Verb::FS_READ, Scope::path("/etc/shadow")).is_err());
        assert!(require(Verb::FS_READ, Scope::path("/etc/hosts")).is_err());
    });
}

#[test]
fn approved_high_risk_request_allows_retry() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"sys.observe","scope":{"kind":"name","value":"**"}}]"#;
    let reg = registry_with_caps("s1", caps);
    let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));

    with_local_invocation("test:high-risk", || {
        let first = require(Verb::SYS_CRASH, Scope::name("system")).unwrap_err();
        assert!(first
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains("pending")));
        let pending = crate::approvals::list_pending();
        assert_eq!(pending.len(), 1);
        crate::approvals::approve(
            &pending[0].id,
            crate::approvals::GrantDuration::Once,
            None,
            None,
        )
        .unwrap();

        assert!(require(Verb::SYS_CRASH, Scope::name("system")).is_ok());
    });
}

#[test]
fn approved_once_grant_allows_exactly_one_denied_request() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s1", caps);
    let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
    with_local_invocation("test:one-shot", || {
        let id = crate::approvals::submit_owned_with_context(
            Verb::FS_WRITE,
            Scope::path("/tmp/granted/file"),
            "s1",
            "test grant",
            None,
            Some(unsafe { libc::geteuid() }),
            Some(crate::caps::ConsentContext::Attended),
        )
        .unwrap();
        crate::approvals::approve(&id, crate::approvals::GrantDuration::Once, None, None).unwrap();

        assert!(require(Verb::FS_WRITE, Scope::path("/tmp/granted/file")).is_ok());
        let err = require(Verb::FS_WRITE, Scope::path("/tmp/granted/file")).unwrap_err();
        assert!(matches!(
            err.reason,
            super::super::denial::DenialReason::VerbNotGranted
        ));
    });
}

#[test]
fn permissive_allows_session_without_caps_field() {
    let _lock = env_lock();
    let reg = r#"{
      "sessions": [{"session_id":"s1","pid":0}]
    }"#;
    let _g = EnvGuard::new(reg, Some("s1"), Some("permissive"));
    assert!(require(Verb::FS_READ, Scope::path("/etc")).is_ok());
}

#[test]
fn strict_is_the_default() {
    let _lock = env_lock();
    let _g = EnvGuard::new(r#"{"sessions":[]}"#, None, None);
    // No COS_PERMS_MODE set → strict by default → denies.
    let err = require(Verb::FS_READ, Scope::path("/etc")).unwrap_err();
    assert!(matches!(
        err.reason,
        super::super::denial::DenialReason::NoSession
    ));
}

#[test]
fn strict_denies_session_without_caps_field() {
    let _lock = env_lock();
    let reg = r#"{
      "sessions": [{"session_id":"s1","pid":0}]
    }"#;
    let _g = EnvGuard::new(reg, Some("s1"), Some("strict"));
    let err = require(Verb::FS_READ, Scope::path("/etc")).unwrap_err();
    assert!(matches!(
        err.reason,
        super::super::denial::DenialReason::VerbNotGranted
    ));
}

#[test]
fn json_envelope_matches_denial_shape() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s1", caps);
    let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
    let err = require_or_json(Verb::FS_DELETE, Scope::path("/etc")).unwrap_err();
    assert_eq!(err["error"], "permission denied");
    assert_eq!(err["verb"], "fs.delete");
}

// ----- system-Agent baseline, end to end ---------------------------------

/// Registry row carrying exactly what `clawd` hands a system Agent
/// owned by an unprivileged account.
fn system_agent_registry(owner_uid: u32, owner_home: &std::path::Path) -> String {
    let caps = crate::clawd::system_caps::system_agent_caps(owner_uid, owner_home);
    registry_with_caps("agent-task", &serde_json::to_string(&caps).unwrap())
}

/// Regression for the root-context ambient grant: a task owned by an
/// unprivileged user executes inside root `clawd`, so the gate — not
/// the process euid — is what keeps it out of `/etc/shadow`, other
/// homes, arbitrary hosts, new processes and the credential store.
#[test]
fn system_agent_task_is_denied_global_files_hosts_processes_and_secrets() {
    let _lock = env_lock();
    let home = tempfile::tempdir().unwrap();
    let owner_home = home.path().join("owner");
    let neighbour = home.path().join("neighbour");
    std::fs::create_dir_all(&owner_home).unwrap();
    std::fs::create_dir_all(&neighbour).unwrap();
    let reg = system_agent_registry(1001, &owner_home);
    let _g = EnvGuard::new(&reg, Some("agent-task"), Some("strict"));

    for path in [
        "/etc/shadow",
        "/etc/sudoers",
        "/proc/1/environ",
        "/root/.bashrc",
    ] {
        assert!(
            require(Verb::FS_READ, Scope::path(path)).is_err(),
            "fs.read {path} must be denied"
        );
    }
    let neighbour_file = neighbour.join("private.txt").to_string_lossy().into_owned();
    assert!(require(Verb::FS_READ, Scope::path(&neighbour_file)).is_err());
    assert!(require(Verb::FS_WRITE, Scope::path("/etc/passwd")).is_err());
    assert!(require(Verb::FS_EXEC, Scope::path("/bin/sh")).is_err());

    assert!(require(Verb::NET_DIAL, Scope::host("evil.example.com")).is_err());
    assert!(require(Verb::NET_DIAL, Scope::host("169.254.169.254")).is_err());
    assert!(require(Verb::NET_DIAL, Scope::host("127.0.0.1:9200")).is_err());
    assert!(require(Verb::NET_DIAL, Scope::Wild).is_err());
    assert!(require(Verb::BROWSER_NAV, Scope::host("evil.example.com")).is_err());

    assert!(require(Verb::PROC_SPAWN, Scope::wild()).is_err());
    assert!(require(Verb::DESKTOP_LAUNCH, Scope::name("terminal")).is_err());

    assert!(require(Verb::SECRET_READ, Scope::name("default/OPENAI_API_KEY")).is_err());
    assert!(require(Verb::SYS_PACKAGE, Scope::name("openssh-server")).is_err());
    assert!(require(Verb::SYS_SERVICE, Scope::name("sshd")).is_err());
    assert!(require(Verb::SYS_IDENTITY, Scope::name("manage")).is_err());
    assert!(require(Verb::SYS_STORAGE, Scope::name("diagnose")).is_err());
    assert!(require(Verb::SYS_MOUNT, Scope::path("/dev/sda1")).is_err());
    assert!(require(Verb::TIME_CRON, Scope::wild()).is_err());
    assert!(require(Verb::IPC_SUBSCRIBE, Scope::name("someone-elses-session")).is_err());

    // Read-only observation is not ambient either when the domain
    // names another principal, another account's units, or the
    // machine's security posture.
    for domain in [
        "desktop",
        "identities",
        "firewall",
        "system-snapshots",
        "user@1002.service",
        "**",
    ] {
        assert!(
            require(Verb::SYS_OBSERVE, Scope::name(domain)).is_err(),
            "sys.observe:{domain} must be denied"
        );
    }
}

/// The same session still runs an ordinary owner-scoped conversation:
/// its own files, its own memory, the model, and the verbs that
/// address no resource at all.
#[test]
fn system_agent_task_keeps_owner_scoped_reads_and_resourceless_work() {
    let _lock = env_lock();
    let home = tempfile::tempdir().unwrap();
    let owner_home = home.path().join("owner");
    std::fs::create_dir_all(&owner_home).unwrap();
    let reg = system_agent_registry(1001, &owner_home);
    let _g = EnvGuard::new(&reg, Some("agent-task"), Some("strict"));

    let notes = owner_home.join("notes.md").to_string_lossy().into_owned();
    assert!(require(Verb::FS_READ, Scope::path(&notes)).is_ok());
    assert!(require(Verb::FS_WRITE, Scope::path(&notes)).is_ok());
    assert!(require(Verb::FS_META, Scope::path(&notes)).is_ok());
    assert!(require(Verb::AI_CHAT, Scope::name("claude-sonnet-4")).is_ok());
    assert!(require(Verb::MEMORY_READ, Scope::self_ref("web")).is_ok());
    assert!(require(Verb::MEMORY_WRITE, Scope::self_ref("web")).is_ok());
    assert!(require(Verb::PROC_OBSERVE, Scope::wild()).is_ok());
    assert!(require(Verb::SYS_OBSERVE, Scope::name("power")).is_ok());
    assert!(require(Verb::SYS_OBSERVE, Scope::name("storage")).is_ok());
    assert!(require(Verb::UI_NOTIFY, Scope::wild()).is_ok());
    assert!(require(Verb::TIME_DELAY, Scope::wild()).is_ok());
    assert!(require(Verb::AGENT_INVOKE, Scope::name("web")).is_ok());
}

/// A user approval moves exactly one resource, exactly once. It never
/// becomes standing authority for the sibling host, path or name, and
/// it is never written back into the session's capability set.
#[test]
fn approved_host_grant_does_not_widen_siblings_or_persist() {
    let _lock = env_lock();
    let home = tempfile::tempdir().unwrap();
    let owner_home = home.path().join("owner");
    std::fs::create_dir_all(&owner_home).unwrap();
    let reg = system_agent_registry(1001, &owner_home);
    let _g = EnvGuard::new(&reg, Some("agent-task"), Some("strict"));

    with_local_invocation("test:host-grant", || {
        assert!(require(Verb::NET_DIAL, Scope::host("api.example.com")).is_err());
        let pending = crate::approvals::list_pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].scope, Scope::host("api.example.com"));
        crate::approvals::approve(
            &pending[0].id,
            crate::approvals::GrantDuration::Once,
            None,
            None,
        )
        .unwrap();

        assert!(require(Verb::NET_DIAL, Scope::host("api.example.com")).is_ok());
        // Siblings stay denied, and the grant is spent.
        assert!(require(Verb::NET_DIAL, Scope::host("evil.example.com")).is_err());
        assert!(require(Verb::NET_DIAL, Scope::Wild).is_err());
        assert!(require(Verb::NET_DIAL, Scope::host("api.example.com")).is_err());
    });
    // Nothing was added to the session itself.
    let caps = crate::clawd::system_caps::system_agent_caps(1001, &owner_home);
    assert!(!caps.covers(&super::super::cap::Cap::new(
        Verb::NET_DIAL,
        Scope::host("api.example.com")
    )));
}

/// End to end for an unattended scheduler snapshot: the session the
/// daemon issued for an approved trigger clears the gate for exactly
/// the executor verb and credential the owner delegated, and for
/// nothing the creating session merely happened to hold.
#[test]
fn delegated_scheduler_session_gates_only_its_approved_authority() {
    let _lock = env_lock();
    let home = tempfile::tempdir().unwrap();
    let owner_home = home.path().join("owner");
    let neighbour = home.path().join("neighbour");
    std::fs::create_dir_all(&owner_home).unwrap();
    std::fs::create_dir_all(&neighbour).unwrap();

    // What `triggers::submit_job` persists, before the clamp.
    let mut stored = crate::clawd::system_caps::system_agent_caps(1001, &owner_home);
    stored.insert(super::super::cap::Cap::new(Verb::AGENT_SPAWN, Scope::Wild));
    stored.insert(super::super::cap::Cap::new(
        Verb::SECRET_READ,
        Scope::name("default/SLACK_TOKEN"),
    ));
    stored.insert(super::super::cap::Cap::new(Verb::PROC_SPAWN, Scope::Wild));
    stored.insert(super::super::cap::Cap::new(
        Verb::SECRET_READ,
        Scope::name("**"),
    ));
    stored.insert(super::super::cap::Cap::new(
        Verb::FS_READ,
        Scope::path("/**"),
    ));
    stored.insert(super::super::cap::Cap::new(
        Verb::SYS_SERVICE,
        Scope::name("sshd"),
    ));

    let delegated = crate::clawd::system_caps::clamp_for_origin(
        &stored,
        crate::session::SessionOrigin::TriggerDelegation,
        1001,
        &owner_home,
    );
    let reg = registry_with_caps("trigger-job", &serde_json::to_string(&delegated).unwrap());
    let _g = EnvGuard::new(&reg, Some("trigger-job"), Some("strict"));

    // Delegated authority still works unattended.
    assert!(require(Verb::AGENT_SPAWN, Scope::wild()).is_ok());
    assert!(require(Verb::SECRET_READ, Scope::name("default/SLACK_TOKEN")).is_ok());
    assert!(require(
        Verb::FS_READ,
        Scope::path(owner_home.join("notes.md").to_string_lossy().into_owned())
    )
    .is_ok());

    // Everything the snapshot carried but never had reviewed is gone.
    assert!(require(Verb::PROC_SPAWN, Scope::wild()).is_err());
    assert!(require(Verb::SECRET_READ, Scope::name("default/OTHER")).is_err());
    assert!(require(Verb::SYS_SERVICE, Scope::name("sshd")).is_err());
    assert!(require(Verb::FS_READ, Scope::path("/etc/shadow")).is_err());
    assert!(require(
        Verb::FS_READ,
        Scope::path(neighbour.join("private.txt").to_string_lossy().into_owned())
    )
    .is_err());
}

// ----- audit-record shape ------------------------------------------------

#[test]
fn audit_record_allow_carries_decision_verb_and_target() {
    let scope = Scope::path("/home/jay/notes.md");
    let rec = build_cap_audit_record(
        Verb::FS_READ,
        &scope,
        Mode::Strict,
        Some("s1"),
        &Ok(()),
        None,
    );
    assert_eq!(rec["decision"], "allow");
    assert_eq!(rec["verb"], "fs.read");
    assert_eq!(rec["session_id"], "s1");
    assert_eq!(rec["mode"], "strict");
    assert_eq!(rec["target_resource"], "/home/jay/notes.md");
    assert_eq!(rec["scope"]["kind"], "path");
    assert_eq!(rec["scope"]["value"], "/home/jay/notes.md");
    assert!(rec["reason"].is_null());
    assert!(rec["hint"].is_null());
    assert_eq!(rec["risk"], "low");
    assert!(rec["approval"].is_null());
}

#[test]
fn audit_record_deny_emits_reason_and_hint() {
    let scope = Scope::path("/etc/passwd");
    let denial = super::super::denial::Denial::verb_not_granted(Verb::FS_DELETE, scope.clone())
        .with_hint("ask the user");
    let rec = build_cap_audit_record(
        Verb::FS_DELETE,
        &scope,
        Mode::Strict,
        None,
        &Err(denial),
        None,
    );
    assert_eq!(rec["decision"], "deny");
    assert_eq!(rec["reason"], "verb-not-granted");
    assert_eq!(rec["hint"], "ask the user");
    assert_eq!(rec["risk"], "high");
    assert!(rec["approval"].is_null());
    assert!(rec["session_id"].is_null());
}

#[test]
fn audit_record_wild_scope_renders_as_star() {
    let scope = Scope::wild();
    let rec = build_cap_audit_record(Verb::FS_READ, &scope, Mode::Permissive, None, &Ok(()), None);
    assert_eq!(rec["target_resource"], "*");
    assert_eq!(rec["scope"]["kind"], "wild");
    assert_eq!(rec["mode"], "permissive");
}

/// Regression alongside `audit::cap_audit_tests`: `require` records
/// through the enforcement path even when the checked process sets
/// the retired `COS_CAPS_AUDIT=0` switch, and it records the allow
/// and the deny alike.
#[test]
fn require_writes_to_caps_jsonl() {
    let _lock = env_lock();
    let _audit_flag = crate::test_env::TestEnvVarGuard::set("COS_CAPS_AUDIT", "0");

    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s1", caps);
    let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));

    // EnvGuard redirects COS_LOG_DIR to its tempdir, so writes
    // land at <tmp>/caps.jsonl. One allow + one deny → two lines.
    let _ = require(Verb::FS_READ, Scope::path("/home/jay/x"));
    let _ = require(Verb::FS_DELETE, Scope::path("/home/jay/x"));

    let body = std::fs::read_to_string(crate::paths::caps_audit_log_path()).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 2, "expected two audit lines, got {body:?}");
    let allow: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    let deny: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(allow["decision"], "allow");
    assert_eq!(allow["verb"], "fs.read");
    assert_eq!(deny["decision"], "deny");
    assert_eq!(deny["verb"], "fs.delete");
    assert_eq!(deny["reason"], "verb-not-granted");
}

// ---------------------------------------------------------------------------
// Worker approval mediation
// ---------------------------------------------------------------------------

/// Stand-in for the `agentd` worker gateway: records what the gate
/// asked and answers with whatever the test configured.
#[derive(Debug)]
struct FakeGateway {
    consume: std::sync::Mutex<Result<bool, String>>,
    request: std::sync::Mutex<Result<crate::caps::PendingApproval, String>>,
    asked: std::sync::Mutex<Vec<(String, String, Option<String>)>>,
    context: crate::caps::ConsentContext,
}

impl FakeGateway {
    fn new(
        consume: Result<bool, String>,
        request: Result<crate::caps::PendingApproval, String>,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            consume: std::sync::Mutex::new(consume),
            request: std::sync::Mutex::new(request),
            asked: std::sync::Mutex::new(Vec::new()),
            context: crate::caps::ConsentContext::Attended,
        })
    }

    fn unattended(
        consume: Result<bool, String>,
        request: Result<crate::caps::PendingApproval, String>,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            consume: std::sync::Mutex::new(consume),
            request: std::sync::Mutex::new(request),
            asked: std::sync::Mutex::new(Vec::new()),
            context: crate::caps::ConsentContext::Unattended,
        })
    }

    fn record(&self, verb: Verb, scope: &Scope, operation_digest: Option<&str>) {
        if let Ok(mut asked) = self.asked.lock() {
            asked.push((
                verb.as_str().to_string(),
                scope.to_string(),
                operation_digest.map(str::to_string),
            ));
        }
    }
}

impl crate::caps::ApprovalGateway for FakeGateway {
    fn context(&self) -> crate::caps::ConsentContext {
        self.context
    }

    fn consume(
        &self,
        verb: Verb,
        scope: &Scope,
        operation_digest: Option<&str>,
    ) -> Result<bool, String> {
        self.record(verb, scope, operation_digest);
        self.consume.lock().unwrap().clone()
    }

    fn request(
        &self,
        verb: Verb,
        scope: &Scope,
        operation_digest: Option<&str>,
    ) -> Result<crate::caps::PendingApproval, String> {
        self.record(verb, scope, operation_digest);
        self.request.lock().unwrap().clone()
    }
}

struct GatewayGuard;

impl Drop for GatewayGuard {
    fn drop(&mut self) {
        crate::caps::approval_gateway::clear_for_test();
    }
}

#[test]
fn a_worker_gate_reaches_consent_through_its_gateway_not_the_store() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s-gw", caps);
    let _g = EnvGuard::new(&reg, Some("s-gw"), Some("strict"));
    let gateway = FakeGateway::new(
        Ok(false),
        Ok(crate::caps::PendingApproval {
            request_id: Some("ap-test".to_string()),
        }),
    );
    crate::caps::approval_gateway::install(gateway.clone());
    let _restore = GatewayGuard;

    let denial =
        require(Verb::FS_DELETE, Scope::path("/home/jay/x")).expect_err("the verb is not granted");
    let hint = denial.hint.unwrap_or_default();
    assert!(hint.contains("ap-test"), "{hint}");

    // The gate consulted the gateway with the exact verb and scope it
    // refused, and nothing reached the local consent store.
    let asked = gateway.asked.lock().unwrap().clone();
    assert!(asked.contains(&(
        "fs.delete".to_string(),
        Scope::path("/home/jay/x").to_string(),
        None,
    )));
    assert!(crate::approvals::list_pending().is_empty());
}

#[test]
fn a_worker_gate_preserves_the_validated_operation_digest() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s-gw-digest", caps);
    let _g = EnvGuard::new(&reg, Some("s-gw-digest"), Some("strict"));
    let gateway = FakeGateway::new(
        Ok(false),
        Ok(crate::caps::PendingApproval {
            request_id: Some("ap-test".to_string()),
        }),
    );
    crate::caps::approval_gateway::install(gateway.clone());
    let _restore = GatewayGuard;
    let digest = crate::crypto::sha256_hex(b"/usr/bin/printf\0hello");

    require_for_operation(
        Verb::PROC_SPAWN,
        Scope::self_ref("children"),
        &digest,
    )
    .expect_err("the capability is not granted");

    assert!(gateway.asked.lock().unwrap().iter().all(
        |(_, _, operation_digest)| operation_digest.as_deref() == Some(digest.as_str())
    ));
}

#[test]
fn an_approved_grant_lets_the_retry_through_the_worker_gateway() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s-gw2", caps);
    let _g = EnvGuard::new(&reg, Some("s-gw2"), Some("strict"));
    let gateway = FakeGateway::new(
        Ok(true),
        Ok(crate::caps::PendingApproval { request_id: None }),
    );
    crate::caps::approval_gateway::install(gateway);
    let _restore = GatewayGuard;

    // The normal post-approval retry: the broker spends the grant
    // one-shot and the gate proceeds.
    assert!(require(Verb::FS_DELETE, Scope::path("/home/jay/x")).is_ok());
}

#[test]
fn an_unavailable_broker_keeps_the_gate_closed() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s-gw3", caps);
    let _g = EnvGuard::new(&reg, Some("s-gw3"), Some("strict"));
    let gateway = FakeGateway::new(
        Err("consent store is unavailable".to_string()),
        Err("consent store is unavailable".to_string()),
    );
    crate::caps::approval_gateway::install(gateway);
    let _restore = GatewayGuard;

    let denial = require(Verb::FS_DELETE, Scope::path("/home/jay/x"))
        .expect_err("mediation failure must not open the gate");
    let hint = denial.hint.unwrap_or_default();
    assert!(hint.contains("could not create approval request"), "{hint}");
}

#[test]
fn an_unattended_worker_fails_closed_without_filing_a_request() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s-unattended", caps);
    let _g = EnvGuard::new(&reg, Some("s-unattended"), Some("strict"));
    let gateway = FakeGateway::unattended(
        Ok(false),
        Err("request should not be attempted".to_string()),
    );
    crate::caps::approval_gateway::install(gateway.clone());
    let _restore = GatewayGuard;

    let denial = require(Verb::FS_DELETE, Scope::path("/home/jay/x"))
        .expect_err("unattended work must not open a prompt");
    assert_eq!(
        denial.approval.as_ref().map(|approval| approval.status),
        Some(crate::caps::ApprovalStatus::RequiredUnattended)
    );
    assert!(denial
        .hint
        .as_deref()
        .is_some_and(|hint| hint.contains("unattended")));
    assert_eq!(
        gateway.asked.lock().unwrap().len(),
        1,
        "only the exact-grant consume probe should reach the broker"
    );
    assert!(crate::approvals::list_pending().is_empty());
}
