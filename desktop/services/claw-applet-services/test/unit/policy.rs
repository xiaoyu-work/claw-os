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
            br#"{"decision":"allow","verb":"sys.observe","scope":{"kind":"wild"}}"#,
            "sys.observe",
            Scope::Wild,
        ),
        Ok(())
    );
}

#[test]
fn parses_deny_decision() {
    assert_eq!(
        parse_decision(
            br#"{"decision":"deny","reason":"capability not granted"}"#,
            "agent.observe",
            Scope::Name("tasks"),
        ),
        Err(Failure::new(FailureKind::Denied, "capability not granted"))
    );
}

#[test]
fn structured_kernel_identity_denials_keep_the_denial_and_localized_summary() {
    let raw = br#"{"decision":"deny","reason":{"pid-ancestry-mismatch":{"caller_pid":42,"session_pid":99}},"summary":"Caller is not in the registered process tree."}"#;
    assert_eq!(
        parse_decision(raw, "clipboard.read", Scope::Name("history")),
        Err(Failure::new(
            FailureKind::Denied,
            "Caller is not in the registered process tree.",
        )),
    );
}

#[test]
fn rejects_malformed_decision() {
    let error =
        parse_decision(br#"{"verb":"sys.observe"}"#, "sys.observe", Scope::Wild).unwrap_err();
    assert!(error.message.contains("returned invalid data"));
    assert_eq!(error.kind, FailureKind::Execution);
}

#[test]
fn allow_requires_the_exact_requested_verb_and_scope() {
    for raw in [
        r#"{"decision":"allow"}"#,
        r#"{"decision":"allow","verb":"clipboard.write","scope":{"kind":"name","value":"history"}}"#,
        r#"{"decision":"allow","verb":"clipboard.read","scope":{"kind":"wild"}}"#,
        r#"{"decision":"allow","verb":"clipboard.read","scope":{"kind":"name","value":"selection"}}"#,
        r#"{"decision":"unknown","verb":"clipboard.read","scope":{"kind":"name","value":"history"}}"#,
    ] {
        assert_eq!(
            parse_decision(raw.as_bytes(), "clipboard.read", Scope::Name("history"))
                .unwrap_err()
                .kind,
            FailureKind::Execution,
        );
    }
    parse_decision(
        br#"{"decision":"allow","verb":"clipboard.read","scope":{"kind":"name","value":"history"}}"#,
        "clipboard.read", Scope::Name("history"),
    ).unwrap();
}

#[cfg(not(feature = "provider"))]
#[test]
fn library_keeps_the_original_os_applet_json_number_behavior() {
    #[derive(Deserialize)]
    #[serde(tag = "kind")]
    enum Sample {
        Percent { value: f32 },
    }
    let Sample::Percent { value } =
        serde_json::from_str(r#"{"kind":"Percent","value":42.5}"#).unwrap();
    assert_eq!(value, 42.5);
}
