use super::*;
use serde_json::json;

#[test]
fn log_cap_decision_writes_under_log_dir() {
    let dir = tempfile::tempdir().unwrap();
    let prev = std::env::var_os("COS_LOG_DIR");
    std::env::set_var("COS_LOG_DIR", dir.path());
    std::env::remove_var("COS_CAPS_AUDIT");
    log_cap_decision(json!({
        "session_id": "s-1",
        "verb": "fs.read",
        "decision": "allow",
    }));
    let p = crate::paths::caps_audit_log_path();
    assert!(p.is_file(), "expected {} to be a file", p.display());
    let body = std::fs::read_to_string(&p).unwrap();
    let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
    assert_eq!(v["session_id"], json!("s-1"));
    assert_eq!(v["verb"], json!("fs.read"));
    assert!(v["timestamp"].is_string());
    match prev {
        Some(v) => std::env::set_var("COS_LOG_DIR", v),
        None => std::env::remove_var("COS_LOG_DIR"),
    }
}

#[test]
fn log_cap_decision_skipped_when_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let prev = std::env::var_os("COS_LOG_DIR");
    std::env::set_var("COS_LOG_DIR", dir.path());
    std::env::set_var("COS_CAPS_AUDIT", "0");
    log_cap_decision(json!({
        "session_id": "s-1",
        "verb": "fs.read",
        "decision": "deny",
    }));
    let p = crate::paths::caps_audit_log_path();
    assert!(!p.exists(), "expected no caps.jsonl to be written");
    std::env::remove_var("COS_CAPS_AUDIT");
    match prev {
        Some(v) => std::env::set_var("COS_LOG_DIR", v),
        None => std::env::remove_var("COS_LOG_DIR"),
    }
}
