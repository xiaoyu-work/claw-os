use super::*;

#[test]
fn validates_credential_broker_components() {
    assert!(validate_component("namespace", "default").is_ok());
    assert!(validate_component("namespace", "../escape").is_err());
}

#[test]
fn requires_non_empty_rpc_strings() {
    let params = json!({"session":"s", "namespace":"default"});
    assert_eq!(required_string(&params, "session").unwrap(), "s");
    assert!(required_string(&params, "credential").is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn filesystem_identity_guard_restores_calling_identity() {
    let uid = unsafe { libc::geteuid() as u32 };
    let gid = unsafe { libc::getegid() as u32 };
    {
        let _guard = FsIdentityGuard::enter(uid, gid).unwrap();
        assert_eq!(
            unsafe { libc::setfsuid(!0 as libc::uid_t) },
            uid as libc::c_int
        );
        assert_eq!(
            unsafe { libc::setfsgid(!0 as libc::gid_t) },
            gid as libc::c_int
        );
    }
    assert_eq!(
        unsafe { libc::setfsuid(!0 as libc::uid_t) },
        uid as libc::c_int
    );
    assert_eq!(
        unsafe { libc::setfsgid(!0 as libc::gid_t) },
        gid as libc::c_int
    );
}

#[test]
fn broker_authorizes_only_the_session_access_token_scope() {
    use crate::caps::{CapSet, Role};
    use crate::proc::{deregister_session, register_session, SessionInfo};

    let _lock = crate::caps::test_env_lock::env_lock();
    let temp = tempfile::tempdir().unwrap();
    let previous_data = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_DATA_DIR", temp.path());
    let session_id = format!("credential-broker-test-{}", std::process::id());
    let mut caps = CapSet::new();
    caps.insert(Cap::new(
        Verb::SECRET_READ,
        Scope::name("default/GOOGLE_ACCESS_TOKEN"),
    ));
    register_session(SessionInfo {
        session_id: session_id.clone(),
        pid: std::process::id(),
        command: vec!["credential-broker-test".into()],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: None,
        parent: None,
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: Some(2),
        scope: None,
        priority: None,
        caps: Some(caps),
        transient_caps: None,
        role: Some(Role::Worker.name().to_string()),
        app_id: Some("email".to_string()),
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
    })
    .unwrap();

    let allowed = authorize_session(
        &session_id,
        std::process::id(),
        "default",
        "GOOGLE_ACCESS_TOKEN",
    );
    let denied = authorize_session(
        &session_id,
        std::process::id(),
        "default",
        "MICROSOFT_ACCESS_TOKEN",
    );

    deregister_session(&session_id);
    match previous_data {
        Some(value) => std::env::set_var("COS_DATA_DIR", value),
        None => std::env::remove_var("COS_DATA_DIR"),
    }

    assert!(allowed.is_ok(), "{allowed:?}");
    let denied = denied.unwrap_err();
    assert!(denied.message.contains("lacks secret.read"));
    assert_eq!(
        denied.kind,
        crate::clawd::protocol::BrokerErrorKind::Unauthorized
    );
    let response = crate::clawd::protocol::Response::handler_error(
        crate::clawd::protocol::RequestId::unknown(),
        denied,
    );
    assert_eq!(response.error.unwrap().code, "not_authorized");
}
