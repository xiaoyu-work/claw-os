use super::*;
use serde_json::json;

#[test]
fn decode_success_envelope() {
    let env = Envelope::decode(json!({
        "ok": true,
        "wire_version": 1,
        "data": {"verb": "fs.read"}
    }))
    .unwrap();
    assert!(env.ok);
    assert_eq!(env.data.unwrap()["verb"], "fs.read");
}

#[test]
fn decode_error_envelope() {
    let env = Envelope::decode(json!({
        "ok": false,
        "wire_version": 1,
        "error": "nope",
        "code": "PERMISSION_DENIED"
    }))
    .unwrap();
    assert!(!env.ok);
    assert_eq!(env.error.as_deref(), Some("nope"));
    assert_eq!(env.code.as_deref(), Some("PERMISSION_DENIED"));
}

#[test]
fn rejects_flat_and_incoherent_envelopes() {
    for value in [
        json!({"verb": "fs.read"}),
        json!({"ok": true, "wire_version": 1, "error": "nope", "code": "INTERNAL_ERROR"}),
        json!({"ok": false, "wire_version": 1, "error": "nope"}),
        json!({"ok": true, "wire_version": 2, "data": {}}),
    ] {
        assert!(Envelope::decode(value).is_err());
    }
}

#[test]
fn error_code_roundtrip() {
    for code in ["PERMISSION_DENIED", "BUDGET_EXCEEDED", "SOMETHING_NEW"] {
        assert_eq!(ErrorCode::from_str_lossy(code).as_str(), code);
    }
}
