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
    assert_eq!(CHILD_PROVIDER_ROUTES.len(), 39);
    assert!(child_route(Command::SystemRegionalSettingsControl.route()));
    assert!(!host_lifecycle_route(
        Command::SystemRegionalSettingsControl
    ));
    assert!(child_route(Command::SystemNotificationControl.route()));
    assert!(!host_lifecycle_route(Command::SystemNotificationControl));
    assert!(!child_route(Command::NotificationList.route()));
    assert!(!child_route(Command::NotificationDismiss.route()));
    assert!(child_route(Command::SystemMediaPlayerControl.route()));
    assert!(!host_lifecycle_route(Command::SystemMediaPlayerControl));
    assert!(child_route(Command::SystemAppPermissions.route()));
    assert!(!child_route(Command::PermissionApps.route()));
    assert!(child_route(Command::SystemFilesystemRead.route()));
    assert!(child_route(Command::AiChat.route()));
    assert!(child_route(Command::SystemFilesystemWrite.route()));
    assert!(child_route(Command::SystemBrowserControl.route()));
    assert!(child_route(Command::SystemNetworkDiagnose.route()));
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

    let app_mcp = session("app-mcp-own", "extension-a", "app-mcp");
    let app_mcp_request = Request::build(
        Command::SystemAudioControl,
        serde_json::json!({"session":"app-mcp-own"}),
    );
    assert!(session_matches_request(
        &app_mcp,
        "extension-a",
        &app_mcp_request
    ));
}

#[test]
fn the_private_broker_lease_binds_both_process_identities() {
    let pid = std::process::id();
    let start = crate::proc::read_start_time_ticks_pub(pid);
    let lease = ExtensionLease::new(
        super::super::protocol::HostPurpose::Task,
        "task-a".to_string(),
        Some("session-a".to_string()),
        Some("extension-a".to_string()),
        unsafe { libc::geteuid() },
        61_184,
        unsafe { libc::getegid() },
        "a".repeat(16),
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

#[test]
fn activity_boundary_is_root_pinned_and_heartbeat_cannot_adopt_a_new_policy() {
    use crate::activities::ActivityService;
    use crate::test_env::TestEnvVarGuard;
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    for introduce in [false, true] {
        let service = crate::activities::open_default().unwrap();
        let activity = service
            .create(
                1000,
                serde_json::from_value(serde_json::json!({
                    "title":"Pinned policy", "goal":"Never replace a running task's boundary"
                }))
                .unwrap(),
            )
            .unwrap();
        let policy = || {
            serde_json::from_value(serde_json::json!({
            "rules":[{"verb":"fs.read","mode":"normal","scopes":[{"kind":"path","value":"/workspace/**"}]}]
        })).unwrap()
        };
        if !introduce {
            service
                .set_capability_policy(1000, &activity.id, None, policy())
                .unwrap();
        }
        let job: crate::agent::service::Job = serde_json::from_value(serde_json::json!({
            "schema_version":2,"execution_phase":"preparing",
            "id":"task-a","prompt":"probe","status":"running","created_at":"2026-01-01T00:00:00Z",
            "owner_uid":1000,"session_id":"session-a","activity_id":activity.id
        }))
        .unwrap();
        let boundary = crate::caps::activity_boundary::ActivityBoundary::for_job(&job)
            .unwrap()
            .unwrap();
        let mut current = lease(super::super::protocol::HostPurpose::Task);
        current.owner_uid = 1000;
        let current = current.with_activity(Some(boundary.clone()));
        assert!(Arc::ptr_eq(
            &current.activity_boundary().unwrap(),
            &boundary
        ));
        current.verify_live().unwrap();
        service
            .set_capability_policy(1000, &activity.id, (!introduce).then_some(1), policy())
            .unwrap();
        current.renew(std::time::Duration::from_secs(60));
        let error = current.verify_live().unwrap_err();
        assert!(error.contains("changed"), "{error}");
        assert!(Arc::ptr_eq(
            &current.activity_boundary().unwrap(),
            &boundary
        ));
        assert_eq!(boundary.binding().revision, (!introduce).then_some(1));
    }
}

#[test]
fn disabled_and_foreign_activity_boundaries_refuse_extension_admission() {
    use crate::activities::ActivityService;
    use crate::test_env::TestEnvVarGuard;
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let service = crate::activities::open_default().unwrap();
    let activity = service
        .create(
            1000,
            serde_json::from_value(serde_json::json!({
                "title":"Bound lease", "goal":"Keep the exact owner and session"
            }))
            .unwrap(),
        )
        .unwrap();
    service
        .set_capability_policy(
            1000,
            &activity.id,
            None,
            serde_json::from_value(serde_json::json!({"rules":[]})).unwrap(),
        )
        .unwrap();
    let job: crate::agent::service::Job = serde_json::from_value(serde_json::json!({
        "schema_version":2,"execution_phase":"preparing",
        "id":"task-a","prompt":"probe","status":"running","created_at":"2026-01-01T00:00:00Z",
        "owner_uid":1000,"session_id":"session-a","activity_id":activity.id
    }))
    .unwrap();
    let boundary = crate::caps::activity_boundary::ActivityBoundary::for_job(&job)
        .unwrap()
        .unwrap();
    for wrong_session in [false, true] {
        let mut foreign = lease(super::super::protocol::HostPurpose::Task);
        foreign.owner_uid = if wrong_session { 1000 } else { 2000 };
        if wrong_session {
            foreign.task_session_id = Some("other-session".into());
        }
        assert!(foreign
            .with_activity(Some(boundary.clone()))
            .verify_live()
            .is_err());
    }
    let mut current = lease(super::super::protocol::HostPurpose::Task);
    current.owner_uid = 1000;
    let current = current.with_activity(Some(boundary));
    service
        .set_capability_policy_enabled(1000, &activity.id, 1, false)
        .unwrap();
    current.renew(std::time::Duration::from_secs(60));
    assert!(current.verify_live().unwrap_err().contains("disabled"));
}

#[test]
fn extension_identity_preserves_owner_and_execution_uid_as_distinct_principals() {
    let pid = std::process::id();
    let start = crate::proc::read_start_time_ticks_pub(pid).unwrap();
    let identity = ClientIdentity::from_verified_delegation(
        pid,
        1000,
        61_000,
        65_534,
        start,
        crate::clawd::client_identity::AuthenticatedExtensionHost {
            purpose: super::super::protocol::HostPurpose::Task,
            lease_id: "task-a".into(),
            authority_session_id: Some("session-a".into()),
            host_session_id: Some("extension-a".into()),
            owner_uid: 1000,
            extension_uid: 61_000,
            capability_generation: "a".repeat(16),
            host_pid: pid,
            host_start_time_ticks: Some(start),
        },
    );
    assert_eq!(identity.require_uid().unwrap(), 1000);
    assert_eq!(identity.process_uid(), Some(61_000));
    assert_eq!(identity.execution_uid, Some(61_000));
    let wire = serde_json::to_value(identity).unwrap();
    assert_eq!(wire["uid"], 1000);
    assert_eq!(wire["execution_uid"], 61_000);
    assert!(wire.get("extension_host").is_none());
    assert!(wire.get("start_time_ticks").is_none());
}

#[test]
fn heartbeat_renewal_reopens_the_same_private_broker_lease() {
    let pid = std::process::id();
    let start = crate::proc::read_start_time_ticks_pub(pid);
    let lease = ExtensionLease::new(
        super::super::protocol::HostPurpose::Task,
        "task-a".to_string(),
        Some("session-a".to_string()),
        Some("extension-a".to_string()),
        unsafe { libc::geteuid() },
        61_184,
        unsafe { libc::getegid() },
        "a".repeat(16),
        pid,
        start,
        pid,
        start,
        crate::agentd::grant::now_ms().saturating_sub(1),
    );
    assert!(lease.verify_live().unwrap_err().contains("expired"));

    let renewed = lease.renew(std::time::Duration::from_secs(60));
    assert_eq!(lease.deadline_ms.load(Ordering::SeqCst), renewed);
    assert!(renewed > crate::agentd::grant::now_ms());
    assert!(lease.verify_live().is_ok());
}

fn lease(purpose: super::super::protocol::HostPurpose) -> ExtensionLease {
    let pid = std::process::id();
    let start = crate::proc::read_start_time_ticks_pub(pid);
    ExtensionLease::new(
        purpose,
        "task-a".to_string(),
        Some("session-a".to_string()),
        Some("extension-a".to_string()),
        unsafe { libc::geteuid() },
        61_000,
        unsafe { libc::getegid() },
        "a".repeat(16),
        pid,
        start,
        pid,
        start,
        crate::agentd::grant::now_ms() + 60_000,
    )
}

#[test]
fn only_a_task_host_can_reach_the_app_service_dispatch_route() {
    let process = peer::PeerProcess {
        pid: std::process::id(),
        uid: unsafe { libc::geteuid() },
        gid: unsafe { libc::getegid() },
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()).unwrap(),
    };
    let request = Request::build(
        Command::AppServiceCall,
        serde_json::json!({
            "app_id": "notes",
            "tool": "list",
            "arguments": {},
            "audit": {}
        }),
    );
    assert!(request_allowed(
        &request,
        process,
        &lease(super::super::protocol::HostPurpose::Task),
    ));
    assert!(!request_allowed(
        &request,
        process,
        &lease(super::super::protocol::HostPurpose::AppService),
    ));
}
