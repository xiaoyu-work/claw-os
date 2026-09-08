use super::*;

#[test]
fn calendar_scope_is_named_not_wildcard() {
    let mut command = Command::new("cos");
    Scope::Name("calendar").append_args(&mut command);
    assert_eq!(
        command.as_std().get_args().collect::<Vec<_>>(),
        ["--name", "calendar"],
    );
}

#[test]
fn parses_allow_decision() {
    assert_eq!(
        parse_decision(
            br#"{"decision":"allow","verb":"sys.observe"}"#,
            "sys.observe"
        ),
        Ok(())
    );
}

#[test]
fn parses_deny_decision() {
    assert_eq!(
        parse_decision(
            br#"{"decision":"deny","reason":"capability not granted"}"#,
            "agent.observe"
        ),
        Err("capability not granted".to_string())
    );
}

#[test]
fn rejects_malformed_decision() {
    let error = parse_decision(br#"{"verb":"sys.observe"}"#, "sys.observe").unwrap_err();
    assert!(error.contains("returned invalid data"));
}
