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
    use crate::caps::CapSet;
    use crate::clawd::authority::{
        authority, Audience, AudienceSet, Binding, Decision, Issuance, Issuer, Presentation,
        Principal, Requirement, Subject, Uses,
    };

    let uid = current_uid();
    authority().clear_for_test();
    let session_id = format!("credential-broker-test-{}", std::process::id());
    let mut caps = CapSet::new();
    caps.insert(Cap::new(
        Verb::SECRET_READ,
        Scope::name("default/GOOGLE_ACCESS_TOKEN"),
    ));
    let (_handle, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id())
                .expect("name the test process"),
            binding: Binding::ProcessTree,
            subject: Subject::session(session_id.clone()).with_app(Some("email".to_string())),
            audience: AudienceSet::one(Audience::Credential),
            caps,
            lifetime: std::time::Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .expect("issue the App session grant");
    let decision = Decision::for_test(
        view,
        "credential.oauth-refresh",
        Audience::Credential,
        Presentation {
            uid,
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            audience: Audience::Credential,
            route: "credential.oauth-refresh",
            session_id: Some(session_id.clone()),
        },
        None,
        &Requirement::RouteDerived,
    );

    let allowed = authorize_session(&decision, "default", "GOOGLE_ACCESS_TOKEN");
    let denied = authorize_session(&decision, "default", "MICROSOFT_ACCESS_TOKEN");

    authority().clear_for_test();

    assert!(allowed.is_ok(), "{allowed:?}");
    let denial = denied.expect_err("an unheld credential scope is refused");
    assert!(denial.message.contains("secret.read"), "{denial:?}");
    assert_eq!(
        denial.kind,
        crate::clawd::protocol::BrokerErrorKind::Unauthorized,
        "an authority refusal keeps its stable public class"
    );
}

#[cfg(unix)]
fn current_uid() -> u32 {
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}
