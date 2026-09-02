use super::*;

fn session(id: &str, parent: &str, group: &str) -> crate::proc::SessionInfo {
    crate::proc::SessionInfo {
        session_id: id.to_string(),
        pid: std::process::id(),
        command: Vec::new(),
        started_at: String::new(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some(group.to_string()),
        parent: Some(parent.to_string()),
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: None,
        scope: None,
        priority: None,
        caps: None,
        transient_caps: None,
        role: None,
        app_id: (group == "app").then(|| "notes".to_string()),
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        client: crate::session::SessionClient::default(),
    }
}

#[test]
fn child_proxy_is_an_explicit_session_route_allowlist() {
    for route in crate::clawd::routes::ROUTES {
        let expected = route.access == Access::User
            && matches!(
                route.authority.subject,
                crate::clawd::authority::SubjectSource::Session
                    | crate::clawd::authority::SubjectSource::PeerSession
            );
        assert_eq!(child_route(route), expected, "route {}", route.name);
    }
    assert_eq!(CHILD_PROVIDER_ROUTES.len(), 29);
    assert!(!child_route(Command::TaskCancel.route()));
    assert!(!child_route(Command::AppSessionRegister.route()));
    assert!(!child_route(Command::PermissionDecide.route()));
}

#[test]
fn host_lifecycle_never_exposes_admin_or_decision_routes() {
    for allowed in [
        Command::AppSessionRegister,
        Command::McpSessionRegister,
        Command::AppSessionBind,
        Command::AppSessionSetTransient,
        Command::AppSessionDeregister,
        Command::PermissionStatus,
    ] {
        assert!(host_lifecycle_route(allowed), "{}", allowed.as_str());
    }
    for refused in [
        Command::PermissionDecide,
        Command::TaskCancel,
        Command::SchedulerRun,
        Command::ContextUpdate,
    ] {
        assert!(!host_lifecycle_route(refused), "{}", refused.as_str());
    }
}

#[test]
fn a_child_cannot_name_the_host_or_a_sibling_session() {
    let own = session("app-own", "extension-a", "app");
    let own_request = Request::build(
        Command::SystemAudioControl,
        serde_json::json!({"session":"app-own"}),
    );
    assert!(session_matches_request(&own, "extension-a", &own_request));

    let host_request = Request::build(
        Command::SystemAudioControl,
        serde_json::json!({"session":"extension-a"}),
    );
    assert!(!session_matches_request(&own, "extension-a", &host_request));

    let sibling_request = Request::build(
        Command::SystemAudioControl,
        serde_json::json!({"session":"app-sibling"}),
    );
    assert!(!session_matches_request(
        &own,
        "extension-a",
        &sibling_request
    ));

    let foreign_parent = session("app-own", "extension-b", "app");
    assert!(!session_matches_request(
        &foreign_parent,
        "extension-a",
        &own_request
    ));
}

#[test]
fn the_private_broker_lease_binds_both_process_identities() {
    let pid = std::process::id();
    let start = crate::proc::read_start_time_ticks_pub(pid);
    let lease = ExtensionLease::new(
        "task-a".to_string(),
        Some("session-a".to_string()),
        Some("extension-a".to_string()),
        unsafe { libc::geteuid() },
        61_184,
        unsafe { libc::getegid() },
        pid,
        start,
        pid,
        start,
        crate::agentd::grant::now_ms() + 60_000,
    );
    assert!(lease.verify_live().is_ok());
    lease.close();
    assert!(lease.verify_live().unwrap_err().contains("closed"));
}
