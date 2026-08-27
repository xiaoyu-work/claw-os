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
    let error = authorize_session(
        "missing-accessibility-session",
        std::process::id(),
        Cap::new(Verb::UI_ACCESSIBILITY, Scope::name("control")),
    )
    .unwrap_err();
    assert_eq!(error.kind, crate::clawd::protocol::BrokerErrorKind::Unauthorized);
    let response = crate::clawd::protocol::Response::handler_error(
        crate::clawd::protocol::RequestId::unknown(),
        error,
    );
    assert_eq!(response.error.unwrap().code, "not_authorized");
}
