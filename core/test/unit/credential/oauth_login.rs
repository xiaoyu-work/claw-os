use super::*;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct FakeStore {
    contains_calls: AtomicUsize,
    load_calls: AtomicUsize,
    last_enforce_tier: AtomicBool,
}

impl super::super::CredentialStore for FakeStore {
    fn contains(&self, _id: &super::super::CredentialId) -> super::super::CredentialResult<bool> {
        self.contains_calls.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }

    fn load(
        &self,
        _id: &super::super::CredentialId,
        enforce_tier: bool,
    ) -> super::super::CredentialResult<String> {
        self.load_calls.fetch_add(1, Ordering::SeqCst);
        self.last_enforce_tier.store(enforce_tier, Ordering::SeqCst);
        Ok("from-store-interface".to_string())
    }

    fn minimum_tier(
        &self,
        _id: &super::super::CredentialId,
    ) -> super::super::CredentialResult<Option<u8>> {
        Ok(Some(0))
    }

    fn store(
        &self,
        _request: super::super::StoreRequest<'_>,
    ) -> super::super::CredentialResult<super::super::StoreResult> {
        unreachable!("configuration lookup is read-only")
    }
}

struct StrictCapabilityEnv {
    previous_data_dir: Option<std::ffi::OsString>,
    previous_log_dir: Option<std::ffi::OsString>,
    previous_session: Option<std::ffi::OsString>,
    previous_mode: Option<std::ffi::OsString>,
    previous_test_setting: Option<std::ffi::OsString>,
    _temp: tempfile::TempDir,
}

impl StrictCapabilityEnv {
    fn new(caps: serde_json::Value) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let proc_dir = temp.path().join("proc");
        std::fs::create_dir_all(&proc_dir).unwrap();
        std::fs::write(
            proc_dir.join("registry.json"),
            serde_json::to_vec(&serde_json::json!({
                "sessions": [{
                    "session_id": "oauth-credential-auth-test",
                    "pid": 0,
                    "caps": caps,
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let previous_data_dir = std::env::var_os("COS_DATA_DIR");
        let previous_log_dir = std::env::var_os("COS_LOG_DIR");
        let previous_session = std::env::var_os("COS_SESSION");
        let previous_mode = std::env::var_os("COS_PERMS_MODE");
        let previous_test_setting = std::env::var_os("COS_TEST_OAUTH_SETTING_DOES_NOT_EXIST");
        std::env::set_var("COS_DATA_DIR", temp.path());
        std::env::set_var("COS_LOG_DIR", temp.path());
        std::env::set_var("COS_SESSION", "oauth-credential-auth-test");
        std::env::set_var("COS_PERMS_MODE", "strict");
        std::env::remove_var("COS_TEST_OAUTH_SETTING_DOES_NOT_EXIST");
        Self {
            previous_data_dir,
            previous_log_dir,
            previous_session,
            previous_mode,
            previous_test_setting,
            _temp: temp,
        }
    }
}

impl Drop for StrictCapabilityEnv {
    fn drop(&mut self) {
        restore_env("COS_DATA_DIR", self.previous_data_dir.take());
        restore_env("COS_LOG_DIR", self.previous_log_dir.take());
        restore_env("COS_SESSION", self.previous_session.take());
        restore_env("COS_PERMS_MODE", self.previous_mode.take());
        restore_env(
            "COS_TEST_OAUTH_SETTING_DOES_NOT_EXIST",
            self.previous_test_setting.take(),
        );
    }
}

fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
    match value {
        Some(value) => std::env::set_var(name, value),
        None => std::env::remove_var(name),
    }
}

fn session(role: crate::caps::Role, app_id: Option<&str>, pid: u32) -> crate::proc::SessionInfo {
    crate::proc::SessionInfo {
        session_id: "oauth-login-test".into(),
        pid,
        command: vec!["cos".into(), "credential".into(), "oauth-login".into()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: None,
        parent: None,
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: Some(role.credential_tier()),
        scope: None,
        priority: None,
        caps: None,
        transient_caps: None,
        role: Some(role.name().to_string()),
        app_id: app_id.map(str::to_string),
        pending_bind: false,
        start_time_ticks: None,
    }
}

#[test]
fn accepts_only_same_pid_admin_cli_session() {
    assert!(is_direct_oauth_login_session(&session(
        crate::caps::Role::Admin,
        None,
        std::process::id(),
    )));
    assert!(!is_direct_oauth_login_session(&session(
        crate::caps::Role::Worker,
        Some("email"),
        std::process::id(),
    )));
    assert!(!is_direct_oauth_login_session(&session(
        crate::caps::Role::Admin,
        None,
        std::process::id() + 1,
    )));
    let mut agent_chat = session(crate::caps::Role::Admin, None, std::process::id());
    agent_chat.command = vec!["cos".into(), "agent".into(), "chat".into()];
    assert!(!is_direct_oauth_login_session(&agent_chat));
    let mut credential_load = session(crate::caps::Role::Admin, None, std::process::id());
    credential_load.command = vec!["cos".into(), "credential".into(), "load".into()];
    assert!(is_same_pid_admin_cli_session(&credential_load));
    assert!(!is_direct_oauth_login_session(&credential_load));
}

#[test]
fn agent_entry_accepts_only_attended_local_agent_sessions() {
    let mut agent_chat = session(crate::caps::Role::Admin, None, std::process::id());
    agent_chat.command = vec!["cos".into(), "agent".into(), "chat".into()];
    assert!(is_attended_agent_oauth_session(&agent_chat));

    let mut mcp_server = session(crate::caps::Role::Admin, None, std::process::id());
    mcp_server.command = vec!["cos".into(), "agent".into(), "mcp".into(), "serve".into()];
    assert!(!is_attended_agent_oauth_session(&mcp_server));

    let mut app_session = session(crate::caps::Role::Worker, Some("email"), std::process::id());
    app_session.command = vec!["cos".into(), "agent".into(), "chat".into()];
    assert!(!is_attended_agent_oauth_session(&app_session));
}

#[test]
fn oauth_token_tiers_separate_app_access_from_refresh_authority() {
    assert_eq!(APP_ACCESS_TOKEN_TIER, 2);
    assert_eq!(REFRESH_TOKEN_TIER, 0);
}

#[test]
fn parses_google_login_arguments() {
    let parsed = parse_args(&[
        "google".into(),
        "--namespace".into(),
        "mail".into(),
        "--no-open".into(),
        "--timeout".into(),
        "60".into(),
    ])
    .unwrap();
    assert_eq!(parsed, ("mail".into(), "google".into(), true, 60));
}

#[test]
fn rejects_missing_oauth_provider() {
    assert!(parse_args(&[]).unwrap_err().contains("usage"));
}

#[test]
fn pkce_values_have_valid_shapes() {
    let (verifier, challenge) = pkce_pair().unwrap();
    assert!((43..=128).contains(&verifier.len()));
    assert_eq!(challenge.len(), 43);
    assert!(!verifier.contains('='));
    assert!(!challenge.contains('='));
}

#[test]
fn authorization_url_uses_pkce_loopback_and_offline_consent() {
    let url = google_authorization_url(
        "client",
        "http://127.0.0.1:1234/oauth/callback",
        "challenge",
        "state",
        "scope one",
    );
    assert!(url.starts_with(GOOGLE_AUTH_URL));
    assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1234%2Foauth%2Fcallback"));
    assert!(url.contains("code_challenge=challenge"));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("access_type=offline"));
    assert!(url.contains("prompt=consent"));
    assert!(url.contains("scope=scope%20one"));
}

#[test]
fn callback_requires_matching_state_and_decodes_code() {
    let code =
        parse_callback_target("/oauth/callback?code=abc%2F123&state=expected", "expected").unwrap();
    assert_eq!(code, "abc/123");
    assert!(
        parse_callback_target("/oauth/callback?code=abc&state=wrong", "expected")
            .unwrap_err()
            .contains("state")
    );
}

#[test]
fn rejects_incomplete_google_granular_consent() {
    let token = serde_json::json!({
        "scope": "openid https://www.googleapis.com/auth/gmail.readonly"
    });
    let error = google_granted_scopes(&token).unwrap_err();
    assert!(error.contains("gmail.send"));
    assert!(error.contains("calendar.events"));
}

#[test]
fn accepts_complete_google_scope_grant() {
    let token = serde_json::json!({"scope": GOOGLE_SCOPES});
    let scopes = google_granted_scopes(&token).unwrap();
    assert!(scopes
        .iter()
        .any(|scope| scope == "https://www.googleapis.com/auth/gmail.send"));
}

#[test]
fn oauth_configuration_uses_credential_store_interface() {
    let _lock = crate::caps::test_env_lock::env_lock();
    let _env = StrictCapabilityEnv::new(serde_json::json!([{
        "verb": "secret.read",
        "scope": {"kind": "name", "value": "default/GOOGLE_CLIENT_ID"},
    }]));
    let store = FakeStore::default();
    let value = client_setting(
        &store,
        "COS_TEST_OAUTH_SETTING_DOES_NOT_EXIST",
        "GOOGLE_CLIENT_ID",
        "default",
    )
    .unwrap();
    assert_eq!(value.as_deref(), Some("from-store-interface"));
    assert_eq!(store.contains_calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.load_calls.load(Ordering::SeqCst), 1);
    assert!(store.last_enforce_tier.load(Ordering::SeqCst));
}

#[test]
fn oauth_configuration_denies_store_probe_without_secret_read() {
    let _lock = crate::caps::test_env_lock::env_lock();
    let _env = StrictCapabilityEnv::new(serde_json::json!([]));
    let store = FakeStore::default();

    let error = client_setting(
        &store,
        "COS_TEST_OAUTH_SETTING_DOES_NOT_EXIST",
        "GOOGLE_CLIENT_ID",
        "default",
    )
    .unwrap_err();

    assert!(error.contains("secret.read"), "{error}");
    assert_eq!(store.contains_calls.load(Ordering::SeqCst), 0);
    assert_eq!(store.load_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn daemon_configuration_uses_only_the_documented_broker_bypass() {
    let _lock = crate::caps::test_env_lock::env_lock();
    let _env = StrictCapabilityEnv::new(serde_json::json!([]));
    let store = FakeStore::default();

    let value = daemon_client_setting(
        &store,
        "COS_TEST_OAUTH_SETTING_DOES_NOT_EXIST",
        "GOOGLE_CLIENT_ID",
        "default",
    )
    .unwrap();

    assert_eq!(value.as_deref(), Some("from-store-interface"));
    assert_eq!(store.contains_calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.load_calls.load(Ordering::SeqCst), 1);
    assert!(!store.last_enforce_tier.load(Ordering::SeqCst));
}
