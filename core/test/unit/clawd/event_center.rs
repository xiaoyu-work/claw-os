use super::*;

#[test]
fn journal_classifier_finds_storage_errors() {
    let value = json!({
        "MESSAGE": "nvme timeout on nvme0",
        "PRIORITY": "3",
    });
    let (source, kind, _) = classify_journal(value).unwrap();
    assert_eq!(source, "storage");
    assert_eq!(kind, "storage.error");
}

#[test]
fn event_sources_are_bounded() {
    validate_action("recent", Some("security"), 10, None).unwrap();
    assert!(validate_action("recent", Some("*"), 10, None).is_err());
}
