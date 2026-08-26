use super::*;

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
