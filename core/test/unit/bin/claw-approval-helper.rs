use super::*;

fn encoded_decision() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": "rv-0123456789abcdef0123456789abcdef",
        "revision": 7,
        "action": "confirm_install",
        "choices": [],
    }))
    .unwrap()
}

#[test]
fn system_review_helper_reads_exact_limit_and_never_reads_beyond_the_extra_byte() {
    let mut bytes = encoded_decision();
    bytes.resize(MAX_DECISION_BYTES, b' ');
    assert_eq!(read_review_decision(bytes.as_slice()).unwrap().revision, 7);
    bytes.resize(MAX_DECISION_BYTES * 2, b' ');
    let mut cursor = std::io::Cursor::new(bytes);
    assert!(read_review_decision(&mut cursor).is_err());
    assert_eq!(cursor.position(), MAX_DECISION_BYTES as u64 + 1);
}

#[test]
fn system_review_helper_rejects_truncation_and_caller_supplied_authority() {
    let bytes = encoded_decision();
    assert!(read_review_decision(&bytes[..bytes.len() - 1]).is_err());
    for key in [
        "owner_uid",
        "approved",
        "grant",
        "permissions_granted",
        "trust",
    ] {
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value[key] = json!(true);
        let encoded = serde_json::to_vec(&value).unwrap();
        assert!(read_review_decision(encoded.as_slice()).is_err(), "{key}");
    }
}

#[test]
fn system_review_helper_rejects_missing_and_duplicate_legacy_arguments() {
    let mut target = None;
    assert!(set_option(&mut target, None, "--id").is_err());
    set_option(&mut target, Some("first".into()), "--id").unwrap();
    assert!(set_option(&mut target, Some("replacement".into()), "--id").is_err());
    assert_eq!(target.as_deref(), Some("first"));
}
