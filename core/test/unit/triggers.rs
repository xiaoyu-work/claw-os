use super::*;

fn ev(source: &str, etype: &str) -> Value {
    json!({ "source": source, "event_type": etype, "payload": {} })
}

fn rule(source: Option<&str>, etype: Option<&str>, contains: Option<&str>) -> TriggerRule {
    TriggerRule {
        id: "r".into(),
        seeded: false,
        enabled: true,
        source: source.map(str::to_string),
        event_type: etype.map(str::to_string),
        contains: contains.map(str::to_string),
        prompt: "do it".into(),
        max_turns: None,
        last_fired_ms: None,
        owner_uid: None,
        owner_home: None,
        owner_caps: None,
        owner_role: None,
        owner_tier: None,
    }
}

#[test]
fn empty_rule_matches_anything() {
    let e = ev("mail", "received");
    assert!(rule_matches(&rule(None, None, None), &e, "{}"));
}

#[test]
fn source_and_type_must_both_match() {
    let e = ev("mail", "received");
    assert!(rule_matches(&rule(Some("mail"), Some("received"), None), &e, "x"));
    assert!(!rule_matches(&rule(Some("mail"), Some("sent"), None), &e, "x"));
    assert!(!rule_matches(&rule(Some("calendar"), None, None), &e, "x"));
}

#[test]
fn contains_checks_raw_line() {
    let e = ev("mail", "received");
    let raw = r#"{"source":"mail","payload":{"from":"boss@x.com"}}"#;
    assert!(rule_matches(&rule(None, None, Some("boss@x.com")), &e, raw));
    assert!(!rule_matches(&rule(None, None, Some("nope")), &e, raw));
}

#[test]
fn sanitize_rejects_traversal_and_dotfiles() {
    assert!(sanitize_id("morning-brief").is_some());
    assert!(sanitize_id("../etc/passwd").is_none());
    assert!(sanitize_id(".hidden").is_none());
    assert!(sanitize_id("").is_none());
}
