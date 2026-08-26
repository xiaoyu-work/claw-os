use super::*;

// Bootstrap mutates global env vars; serialise tests on the
// shared `caps::test_env_lock` so they don't race other modules
// (notably `caps::enforcement`) that mutate the same variables.
use crate::caps::test_env_lock::env_lock;

/// Returns a fresh tempdir and sets `COS_DATA_DIR` to point at it.
/// Restores any previous value when the guard is dropped.
struct DataDirGuard {
    prev: Option<std::ffi::OsString>,
    _tmp: tempfile::TempDir,
}
impl Drop for DataDirGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => env::set_var("COS_DATA_DIR", v),
            None => env::remove_var("COS_DATA_DIR"),
        }
    }
}
fn redirect_data_dir() -> DataDirGuard {
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev = env::var_os("COS_DATA_DIR");
    env::set_var("COS_DATA_DIR", tmp.path());
    DataDirGuard { prev, _tmp: tmp }
}

#[test]
fn bootstrap_is_noop_when_session_already_set() {
    let _lock = env_lock();
    let _data = redirect_data_dir();
    let prev = env::var("COS_SESSION").ok();
    env::set_var("COS_SESSION", "outer-session-id");

    let guard = bootstrap_user_cli_session_impl(&["agent".into()], true);
    assert!(guard.is_none(), "should not bootstrap when COS_SESSION is set");
    assert_eq!(env::var("COS_SESSION").unwrap(), "outer-session-id");

    match prev {
        Some(v) => env::set_var("COS_SESSION", v),
        None => env::remove_var("COS_SESSION"),
    }
}

#[test]
fn bootstrap_registers_session_and_grants_ai_chat() {
    let _lock = env_lock();
    let _data = redirect_data_dir();
    env::remove_var("COS_SESSION");
    env::remove_var("COS_PERMS_MODE");

    let guard =
        bootstrap_user_cli_session_impl(&["agent".into()], true)
            .expect("bootstrap should succeed");
    let sid = env::var("COS_SESSION").expect("COS_SESSION should be set");
    assert!(sid.starts_with("cli-"), "session id format: {sid}");

    // The freshly-bootstrapped session must satisfy the caps gate
    // for ai.chat — the failure mode that motivated this whole
    // module. We can't call `caps::require` directly here because
    // it requires the audit log dir to exist and resolves the
    // session via the same registry we just wrote, so we just
    // sanity-check that the admin verbs are present in the row.
    use crate::caps::Verb;
    let reg_path = crate::paths::data_dir().join("proc/registry.json");
    let raw = std::fs::read_to_string(&reg_path).expect("registry exists");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let caps = v["sessions"][0]["caps"].as_array().expect("caps array");
    let has_ai_chat = caps.iter().any(|c| {
        c["verb"].as_str() == Some(Verb::AI_CHAT.as_str())
    });
    assert!(has_ai_chat, "admin session should hold ai.chat");

    drop(guard);
    // After drop the row is gone and COS_SESSION is cleared.
    assert!(env::var("COS_SESSION").is_err());
    let raw_after = std::fs::read_to_string(&reg_path).unwrap();
    let v_after: serde_json::Value = serde_json::from_str(&raw_after).unwrap();
    assert_eq!(
        v_after["sessions"].as_array().map(|a| a.len()).unwrap_or(0),
        0
    );
}

#[test]
fn bootstrapped_admin_cli_can_store_credentials_and_enter_oauth_login() {
    let _lock = env_lock();
    let _data = redirect_data_dir();
    let credentials = tempfile::tempdir().unwrap();
    let previous_credentials = env::var_os("COS_CREDENTIALS_DIR");
    env::set_var("COS_CREDENTIALS_DIR", credentials.path());
    env::remove_var("COS_SESSION");
    env::remove_var("COS_PERMS_MODE");

    let guard = bootstrap_user_cli_session_impl(
        &["credential".into(), "oauth-login".into(), "google".into()],
        true,
    )
    .expect("interactive credential CLI should bootstrap");
    let stored = crate::credential::run(
        "store",
        &["BOOTSTRAP_TEST".into(), "value".into()],
    );
    let login = crate::credential::run(
        "oauth-login",
        &["google".into(), "--no-open".into(), "--timeout".into(), "30".into()],
    )
    .unwrap_err();

    drop(guard);
    match previous_credentials {
        Some(value) => env::set_var("COS_CREDENTIALS_DIR", value),
        None => env::remove_var("COS_CREDENTIALS_DIR"),
    }

    assert!(stored.is_ok(), "{stored:?}");
    assert!(login.contains("Google OAuth client is not configured"));
    assert!(!login.contains("must be run directly"));
}

/// Per-user data dir override: sets `COS_USER_DATA_DIR` to a
/// fresh tempdir so the per-user-default test isn't subject to
/// the real `$HOME/.local/share/cos` contents.
struct UserDataDirGuard {
    prev: Option<std::ffi::OsString>,
    _tmp: tempfile::TempDir,
}
impl Drop for UserDataDirGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => env::set_var("COS_USER_DATA_DIR", v),
            None => env::remove_var("COS_USER_DATA_DIR"),
        }
    }
}
fn redirect_user_data_dir() -> UserDataDirGuard {
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev = env::var_os("COS_USER_DATA_DIR");
    env::set_var("COS_USER_DATA_DIR", tmp.path());
    UserDataDirGuard { prev, _tmp: tmp }
}

/// When `COS_DATA_DIR` is unset (the normal case for a user
/// invoking `cos` from a shell), the bootstrapped session row
/// must land in the per-user data dir — *not* `/var/lib/cos`,
/// which is `root:root 0755` on a real install. This is the
/// regression that produced
///   `Permission denied (no active session): secret.write on *`
/// when `cos agent setup text` tried to store the GitHub Copilot
/// OAuth token.
#[test]
fn bootstrap_writes_to_user_data_dir_when_cos_data_dir_unset() {
    let _lock = env_lock();
    let prev_data = env::var_os("COS_DATA_DIR");
    env::remove_var("COS_DATA_DIR");
    let user_data = redirect_user_data_dir();
    env::remove_var("COS_SESSION");
    env::remove_var("COS_PERMS_MODE");

    let guard = bootstrap_user_cli_session_impl(&["agent".into()], true)
        .expect("bootstrap should succeed in per-user data dir");
    let sid = env::var("COS_SESSION").expect("COS_SESSION should be set");

    // The row must exist under the user data dir, not under any
    // /var/lib/cos path. Reading user_data_dir() directly to avoid
    // depending on the test guard's internal tempdir handle.
    let user_reg = crate::paths::user_data_dir().join("proc/registry.json");
    let raw = std::fs::read_to_string(&user_reg)
        .expect("user-data-dir registry should exist after bootstrap");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let sessions = v["sessions"].as_array().expect("sessions array");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["session_id"].as_str(), Some(sid.as_str()));

    // Caps gate test: the row must still grant ai.chat so a real
    // `cos agent chat` invocation succeeds end-to-end.
    use crate::caps::Verb;
    let caps = sessions[0]["caps"].as_array().expect("caps array");
    let has_ai_chat = caps
        .iter()
        .any(|c| c["verb"].as_str() == Some(Verb::AI_CHAT.as_str()));
    assert!(has_ai_chat, "user-dir session should hold ai.chat");

    drop(guard);
    assert!(env::var("COS_SESSION").is_err());
    let raw_after = std::fs::read_to_string(&user_reg).unwrap();
    let v_after: serde_json::Value = serde_json::from_str(&raw_after).unwrap();
    assert_eq!(
        v_after["sessions"].as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "user-dir registry should be empty after the guard drops"
    );

    drop(user_data);
    match prev_data {
        Some(v) => env::set_var("COS_DATA_DIR", v),
        None => env::remove_var("COS_DATA_DIR"),
    }
}

/// Sanity check that `caps::require` succeeds for a session
/// living in the per-user registry. Guards against future
/// regressions where `caps::enforcement` accidentally hard-codes
/// `/var/lib/cos` for its registry read.
#[test]
fn enforcement_reads_per_user_registry() {
    let _lock = env_lock();
    let prev_data = env::var_os("COS_DATA_DIR");
    env::remove_var("COS_DATA_DIR");
    let _user_data = redirect_user_data_dir();
    env::remove_var("COS_SESSION");
    env::set_var("COS_PERMS_MODE", "strict");

    let guard =
        bootstrap_user_cli_session_impl(&["agent".into()], true)
            .expect("bootstrap should succeed");
    // require() must see the per-user row.
    use crate::caps::Verb;
    let result = crate::caps::require(Verb::AGENT_INVOKE, Scope::Wild);
    assert!(
        result.is_ok(),
        "expected per-user-registry session to be granted; got {result:?}"
    );

    drop(guard);
    env::remove_var("COS_PERMS_MODE");
    match prev_data {
        Some(v) => env::set_var("COS_DATA_DIR", v),
        None => env::remove_var("COS_DATA_DIR"),
    }
}
