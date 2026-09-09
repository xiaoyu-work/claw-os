use super::*;

#[test]
fn manifest_defaults_preserve_presentation_but_never_supply_authority() {
    let value = request("notify.post", json!({"summary":"Hello"})).unwrap();
    assert_eq!(
        value,
        json!({
            "action":"post","summary":"Hello","body":"","app_name":"Claw OS Agent",
            "icon":"com.clawos.Notifications","expire_ms":-1,"transient":false,
        })
    );
    let custom = request("notify.post", json!({
        "summary":"Hello","app_name":"","icon":"","expire_ms":0,"transient":true,"dedupe_key":"one",
    })).unwrap();
    assert_eq!(custom["app_name"], "Claw OS Agent");
    assert_eq!(custom["icon"], "");
    assert_eq!(custom["expire_ms"], 0);
    assert_eq!(custom["transient"], true);
    assert_eq!(custom["dedupe_key"], "one");
    for field in [
        "owner_uid",
        "source",
        "session_id",
        "task_id",
        "app",
        "actions",
    ] {
        let mut input = json!({"summary":"Hello"});
        input[field] = json!("forged");
        assert!(request("notify.post", input).is_err(), "{field}");
    }
}

#[test]
fn invalid_arguments_fail_before_transport() {
    for (field, value) in [
        ("summary", json!(" ")),
        ("summary", json!("界".repeat(241))),
        ("body", json!("x".repeat(4001))),
        ("body", json!(3)),
        ("expire_ms", json!(-2)),
        ("expire_ms", json!(2147483648_u64)),
        ("expire_ms", json!("0")),
        ("transient", json!("true")),
        ("icon", json!("/etc/shadow")),
        ("icon", json!("file:///private")),
        ("icon", json!("../icon")),
        ("app_name", json!("spoof\nline")),
        ("dedupe_key", json!("")),
    ] {
        let mut input = json!({"summary":"Hello"});
        input[field] = value;
        assert!(request("notify.post", input).is_err(), "{field}");
    }
    for id in [json!(1), json!("1"), json!("../state"), json!("notif-no")] {
        assert!(request("notify.close", json!({"id":id})).is_err());
    }
    let id = format!("notif-{}", "a".repeat(32));
    assert_eq!(request("notify.close", json!({"id":id})).unwrap()["id"], id);
}
