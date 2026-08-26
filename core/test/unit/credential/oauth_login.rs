use super::*;

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
    let code = parse_callback_target(
        "/oauth/callback?code=abc%2F123&state=expected",
        "expected",
    )
    .unwrap();
    assert_eq!(code, "abc/123");
    assert!(parse_callback_target(
        "/oauth/callback?code=abc&state=wrong",
        "expected"
    )
    .unwrap_err()
    .contains("state"));
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
