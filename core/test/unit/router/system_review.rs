use super::*;

fn response() -> Value {
    let manifest = crate::caps::Manifest::from_json(
        r#"{"id":"review-app","version":"1.0.0","name":{"en":"Review App"}}"#,
    )
    .unwrap();
    json!({
        "id": "rv-0123456789abcdef0123456789abcdef",
        "kind": "app-install",
        "state": "pending",
        "permission_review": PermissionReview::from_manifest(&manifest).unwrap(),
        "package": {
            "kind": "app", "id": "review-app", "content_digest": "sha256:fixture",
            "tier": "user"
        }
    })
}

#[test]
fn system_review_terminal_rejects_malformed_and_grant_shaped_views() {
    assert!(parse_review(response()).is_ok());
    let mut malformed = response();
    malformed["id"] = json!("rv-\u{1b}[2J");
    assert!(parse_review(malformed).is_err());
    let mut grant = response();
    grant["permission_review"]["permissions_granted"] = json!(true);
    assert!(parse_review(grant).is_err());
    let mut wrong_app = response();
    wrong_app["package"]["id"] = json!("another-app");
    assert!(parse_review(wrong_app).is_err());
}

#[test]
fn system_review_terminal_does_not_offer_a_yes_flag_for_decisions() {
    let error = dispatch(&[
        "approve".into(),
        "rv-0123456789abcdef0123456789abcdef".into(),
        "--yes".into(),
    ])
    .unwrap_err();
    assert!(error.contains("usage"));
}

#[test]
fn system_review_schema_is_not_a_model_callable_approval_tool() {
    let output = dispatch(&["approve".into(), "--schema".into()])
        .unwrap()
        .unwrap();
    let schema: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(schema["model_callable"], false);
    assert_eq!(schema["command"], "cos review approve");
}
