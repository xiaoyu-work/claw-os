use super::*;
use serde_json::json;

#[test]
fn accept_flat_success() {
    let env = Envelope::accept(json!({"decision": "allow", "verb": "fs.read"}));
    assert!(env.ok);
    assert_eq!(env.wire_version, 1);
    assert_eq!(env.data.unwrap()["decision"], "allow");
}

#[test]
fn accept_flat_error() {
    let env = Envelope::accept(json!({"error": "nope", "code": "PERMISSION_DENIED"}));
    assert!(!env.ok);
    assert_eq!(env.error.as_deref(), Some("nope"));
    assert_eq!(env.code.as_deref(), Some("PERMISSION_DENIED"));
}

#[test]
fn accept_native_v1() {
    let env = Envelope::accept(json!({
        "ok": true,
        "wire_version": 1,
        "data": {"verb": "fs.read"}
    }));
    assert!(env.ok);
}

#[test]
fn error_code_roundtrip() {
    for code in ["PERMISSION_DENIED", "BUDGET_EXCEEDED", "SOMETHING_NEW"] {
        assert_eq!(ErrorCode::from_str_lossy(code).as_str(), code);
    }
}
