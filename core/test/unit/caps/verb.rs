use super::*;

#[test]
fn parse_known_verb() {
    assert_eq!(Verb::parse("fs.read"), Some(Verb::FS_READ));
    assert_eq!(
        Verb::parse("device.microphone"),
        Some(Verb::DEVICE_MICROPHONE)
    );
}

#[test]
fn parse_unknown_verb_is_none() {
    assert_eq!(Verb::parse("fs.unknown"), None);
    assert_eq!(Verb::parse(""), None);
    assert_eq!(Verb::parse("FS.READ"), None); // case-sensitive
}

#[test]
fn all_verbs_are_unique() {
    let mut seen = std::collections::HashSet::new();
    for v in ALL_VERBS {
        assert!(seen.insert(v.as_str()), "duplicate verb: {}", v.as_str());
    }
}

#[test]
fn all_verbs_round_trip_through_parse() {
    for v in ALL_VERBS {
        assert_eq!(Verb::parse(v.as_str()), Some(*v));
    }
}

#[test]
fn display_matches_as_str() {
    assert_eq!(Verb::FS_READ.to_string(), "fs.read");
}

#[test]
fn serde_round_trip() {
    let v = Verb::NET_DIAL;
    let json = serde_json::to_string(&v).unwrap();
    assert_eq!(json, "\"net.dial\"");
    let back: Verb = serde_json::from_str(&json).unwrap();
    assert_eq!(back, v);
}

#[test]
fn serde_accepts_owned_json_value() {
    let back: Verb = serde_json::from_value(serde_json::json!("net.dial")).unwrap();
    assert_eq!(back, Verb::NET_DIAL);
}

#[test]
fn serde_rejects_unknown_verb() {
    let result: Result<Verb, _> = serde_json::from_str("\"fs.totally-not-real\"");
    assert!(result.is_err());
}
