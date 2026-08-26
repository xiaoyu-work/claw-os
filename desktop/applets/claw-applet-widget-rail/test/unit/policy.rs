use super::*;

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
