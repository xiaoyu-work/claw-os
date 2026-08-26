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
    let err = require(Verb::FS_DELETE, Scope::path("/home/jay/x")).unwrap_err();
    assert!(matches!(
        err.reason,
        super::super::denial::DenialReason::VerbNotGranted
    ));
    assert!(err.hint.as_deref().is_some_and(|hint| {
        hint.contains("approval request") && hint.contains("pending")
    }));
    let pending = crate::approvals::list_pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].verb, Verb::FS_DELETE.as_str());
    assert_eq!(pending[0].scope, Scope::path("/home/jay/x"));
}

#[test]
fn low_risk_denial_does_not_create_approval_request() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s1", caps);
    let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
    let err = require(Verb::NET_RESOLVE, Scope::host("example.com")).unwrap_err();
    assert!(err.hint.is_none());
    assert!(crate::approvals::list_pending().is_empty());
}

#[test]
fn approved_high_risk_request_allows_retry() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"sys.observe","scope":{"kind":"name","value":"**"}}]"#;
    let reg = registry_with_caps("s1", caps);
    let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));

    let first = require(Verb::SYS_CRASH, Scope::name("system")).unwrap_err();
    assert!(first.hint.as_deref().is_some_and(|hint| hint.contains("pending")));
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
}

#[test]
fn approved_once_grant_allows_exactly_one_denied_request() {
    let _lock = env_lock();
    let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
    let reg = registry_with_caps("s1", caps);
    let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
    let id = crate::approvals::submit(
        Verb::FS_WRITE,
        Scope::path("/tmp/granted/**"),
        "s1",
        "test grant",
        None,
    )
    .unwrap();
    crate::approvals::approve(&id, crate::approvals::GrantDuration::Once, None, None).unwrap();

    assert!(require(Verb::FS_WRITE, Scope::path("/tmp/granted/file")).is_ok());
    let err = require(Verb::FS_WRITE, Scope::path("/tmp/granted/file")).unwrap_err();
    assert!(matches!(
        err.reason,
        super::super::denial::DenialReason::VerbNotGranted
    ));
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

// ----- audit-record shape ------------------------------------------------

#[test]
fn audit_record_allow_carries_decision_verb_and_target() {
    let scope = Scope::path("/home/jay/notes.md");
    let rec = build_cap_audit_record(Verb::FS_READ, &scope, Mode::Strict, Some("s1"), &Ok(()));
    assert_eq!(rec["decision"], "allow");
    assert_eq!(rec["verb"], "fs.read");
    assert_eq!(rec["session_id"], "s1");
    assert_eq!(rec["mode"], "strict");
    assert_eq!(rec["target_resource"], "/home/jay/notes.md");
    assert_eq!(rec["scope"]["kind"], "path");
    assert_eq!(rec["scope"]["value"], "/home/jay/notes.md");
    assert!(rec["reason"].is_null());
    assert!(rec["hint"].is_null());
}

#[test]
fn audit_record_deny_emits_reason_and_hint() {
    let scope = Scope::path("/etc/passwd");
    let denial = super::super::denial::Denial::verb_not_granted(Verb::FS_DELETE, scope.clone())
        .with_hint("ask the user");
    let rec = build_cap_audit_record(Verb::FS_DELETE, &scope, Mode::Strict, None, &Err(denial));
    assert_eq!(rec["decision"], "deny");
    assert_eq!(rec["reason"], "verb-not-granted");
    assert_eq!(rec["hint"], "ask the user");
    assert!(rec["session_id"].is_null());
}

#[test]
fn audit_record_wild_scope_renders_as_star() {
    let scope = Scope::wild();
    let rec = build_cap_audit_record(Verb::FS_READ, &scope, Mode::Permissive, None, &Ok(()));
    assert_eq!(rec["target_resource"], "*");
    assert_eq!(rec["scope"]["kind"], "wild");
    assert_eq!(rec["mode"], "permissive");
}

#[test]
fn require_writes_to_caps_jsonl() {
    let _lock = env_lock();
    let prev_audit = std::env::var_os("COS_CAPS_AUDIT");
    std::env::remove_var("COS_CAPS_AUDIT");

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

    match prev_audit {
        Some(v) => std::env::set_var("COS_CAPS_AUDIT", v),
        None => std::env::remove_var("COS_CAPS_AUDIT"),
    }
}
