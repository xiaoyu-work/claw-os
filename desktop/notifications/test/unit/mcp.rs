use super::*;

#[test]
fn sender_name_uses_manifest_key_and_default() {
    assert_eq!(
        sender_name(&json!({ "app_name": "Scheduler" })),
        "Scheduler"
    );
    assert_eq!(sender_name(&json!({ "app": "Legacy" })), "Claw OS Agent");
    assert_eq!(sender_name(&json!({ "app_name": "" })), "Claw OS Agent");
    assert_eq!(sender_name(&json!({})), "Claw OS Agent");
}
