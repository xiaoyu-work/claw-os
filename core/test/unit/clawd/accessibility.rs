use super::*;

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
fn missing_accessibility_session_is_an_authorization_failure() {
    // The session lookup now lives in the broker's authority, so what
    // this exercises is the surviving property: a refusal from the
    // decision keeps the stable `not_authorized` code issue #39 gave
    // it, rather than collapsing into a generic execution failure.
    use crate::clawd::authority::{
        authority, Audience, AudienceSet, Binding, Decision, Issuance, Issuer, Presentation,
        Principal, Requirement, Subject, Uses,
    };

    let store = authority();
    store.clear_for_test();
    let uid = unsafe { libc::geteuid() };
    let (_handle, view) = store
        .issue(Issuance {
            issuer: Issuer::AppSessionAuthority,
            principal: Principal::of_process(uid, std::process::id()).expect("this process"),
            binding: Binding::ProcessTree,
            subject: Subject::session("missing-accessibility-session")
                .with_app(Some("accessibility-manager".to_string())),
            audience: AudienceSet::one(Audience::SystemService),
            // Holds nothing, so the exact capability below is refused.
            caps: crate::caps::CapSet::from_caps([Cap::new(
                Verb::SYS_OBSERVE,
                Scope::name("accessibility"),
            )]),
            lifetime: std::time::Duration::from_secs(60),
            uses: Uses::Unbounded,
            index_session: true,
        })
        .expect("issue a session grant");
    let decision = Decision::for_test(
        view,
        "system.accessibility.control",
        Audience::SystemService,
        Presentation {
            uid,
            pid: std::process::id(),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            audience: Audience::SystemService,
            route: "system.accessibility.control",
            session_id: Some("missing-accessibility-session".to_string()),
        },
        None,
        &Requirement::RouteDerived,
    );

    let error = authorize_session(
        &decision,
        Cap::new(Verb::UI_ACCESSIBILITY, Scope::name("control")),
    )
    .unwrap_err();
    assert_eq!(
        error.kind,
        crate::clawd::protocol::BrokerErrorKind::Unauthorized
    );
    let response = crate::clawd::protocol::Response::handler_error(
        crate::clawd::protocol::RequestId::unknown(),
        error,
    );
    assert_eq!(response.error.unwrap().code, "not_authorized");
    store.clear_for_test();
}
