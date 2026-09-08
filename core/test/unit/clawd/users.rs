use super::*;

#[test]
fn status_requires_only_identity_observation() {
    assert_eq!(
        requested_caps("status", None),
        [Cap::new(Verb::SYS_OBSERVE, Scope::name("identities"))]
    );
}

#[test]
fn mutations_retain_identity_management_and_exact_password_scope() {
    for action in [
        "create-user", "delete-user", "lock-user", "unlock-user", "set-shell",
        "create-group", "delete-group", "add-to-group", "remove-from-group", "restore",
    ] {
        assert_eq!(
            requested_caps(action, None),
            [Cap::new(Verb::SYS_IDENTITY, Scope::name("manage"))],
            "{action}"
        );
    }
    let credential = ("default".to_string(), "alice-password".to_string());
    assert_eq!(
        requested_caps("set-password", Some(&credential)),
        [
            Cap::new(Verb::SYS_IDENTITY, Scope::name("manage")),
            Cap::new(Verb::SECRET_READ, Scope::name("default/alice-password")),
        ]
    );
}

#[test]
fn account_names_are_strict() {
    validate_account_name("user", "alice").unwrap();
    assert!(validate_account_name("user", "Alice").is_err());
    assert!(validate_account_name("user", "../root").is_err());
}

#[test]
fn passwords_are_never_allowed_to_span_lines() {
    validate_password("correct horse battery staple").unwrap();
    assert!(validate_password("line1\nline2").is_err());
}
