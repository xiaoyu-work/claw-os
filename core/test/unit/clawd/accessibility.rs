use super::*;
use crate::clawd::authority::{
    authority, Audience, AudienceSet, Binding, Issuance, Issuer, Presentation, Principal,
    Requirement, Subject, Uses,
};
use crate::test_env::TestEnvVarGuard;

fn decision(app: &str, caps: Vec<Cap>, presented_uid: u32) -> Decision {
    let uid = unsafe { libc::geteuid() };
    let pid = std::process::id();
    let session = format!("accessibility-fixture-{}", uuid::Uuid::new_v4().simple());
    let (_, view) = authority()
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, pid).unwrap(),
            binding: Binding::ProcessTree,
            subject: Subject::session(&session).with_app(Some(app.to_string())),
            audience: AudienceSet::one(Audience::SystemService),
            caps: crate::caps::CapSet::from_caps(caps),
            lifetime: Duration::from_secs(60),
            uses: Uses::Budget(1),
            index_session: true,
        })
        .unwrap();
    Decision::for_test(
        view,
        "system.accessibility.control",
        Audience::SystemService,
        Presentation {
            uid: presented_uid,
            pid,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
            audience: Audience::SystemService,
            route: "system.accessibility.control",
            session_id: Some(session),
        },
        None,
        &Requirement::RouteDerived,
    )
}

#[test]
fn accessibility_actions_are_bounded() {
    validate_action("screen-reader", Some("on")).unwrap();
    validate_action("filter", Some("deuteranopia")).unwrap();
    assert!(validate_action("filter", Some("custom")).is_err());
}

#[test]
fn busctl_booleans_are_parsed() {
    assert_eq!(parse_busctl_bool("b true\n"), Some(true));
    assert_eq!(parse_busctl_bool("b false\n"), Some(false));
}

#[test]
fn origin_neutral_accessibility_keeps_live_exact_rights_and_owner_binding() {
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::Builder::new()
        .prefix(".origin-neutral-accessibility-")
        .tempdir_in(std::env::current_dir().unwrap())
        .unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", directory.path());
    let _caps = TestEnvVarGuard::set("COS_CAPS_DATA_DIR", directory.path());
    let uid = unsafe { libc::geteuid() };
    let observe = Cap::new(Verb::SYS_OBSERVE, Scope::name("accessibility"));
    let control = Cap::new(Verb::UI_ACCESSIBILITY, Scope::name("control"));
    for app in ["accessibility-manager", "independent-accessibility-client"] {
        let allowed = decision(app, vec![control.clone()], uid);
        assert!(authorize_session(
            &allowed,
            Cap::new(Verb::UI_ACCESSIBILITY, Scope::name("other"))
        )
        .is_err());
        let proof = authorize_session(&allowed, control.clone()).unwrap();
        assert_eq!(proof.spent(), std::slice::from_ref(&control));
        assert_eq!(allowed.app_id(), Some(app));
        assert!(authorize_session(&allowed, control.clone()).is_err());

        let read_only = decision(app, vec![observe.clone()], uid);
        let error = authorize_session(&read_only, control.clone()).unwrap_err();
        assert_eq!(
            error.kind,
            crate::clawd::protocol::BrokerErrorKind::Unauthorized
        );
        let response = crate::clawd::protocol::Response::handler_error(
            crate::clawd::protocol::RequestId::unknown(),
            error,
        );
        assert_eq!(response.error.unwrap().code, "not_authorized");
        assert!(!crate::clawd::authority::obligation_met(Some(&read_only)));
        let proof = authorize_session(&read_only, observe.clone()).unwrap();
        assert_eq!(proof.spent(), std::slice::from_ref(&observe));

        let missing = decision(app, vec![], uid);
        assert!(authorize_session(&missing, control.clone()).is_err());
        authority().revoke_session(missing.session_id().unwrap());
    }

    let wrong_owner = decision(
        "independent-accessibility-client",
        vec![control.clone()],
        uid + 1,
    );
    assert!(authorize_session(&wrong_owner, control.clone()).is_err());
    assert!(!crate::clawd::authority::obligation_met(Some(&wrong_owner)));
    authority().revoke_session(wrong_owner.session_id().unwrap());
    let revoked = decision(
        "independent-accessibility-client",
        vec![control.clone()],
        uid,
    );
    authority().revoke_session(revoked.session_id().unwrap());
    assert!(authorize_session(&revoked, control).is_err());
}
