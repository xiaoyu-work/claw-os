use super::*;

#[test]
fn sender_name_accepts_manifest_and_legacy_keys() {
    assert_eq!(
        sender_name(&json!({ "app_name": "Scheduler" })),
        "Scheduler"
    );
    assert_eq!(sender_name(&json!({ "app": "Agent" })), "Agent");
    assert_eq!(sender_name(&json!({})), "Claw OS Agent");
}
