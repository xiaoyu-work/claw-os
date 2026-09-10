use super::*;
use clawd_client::system_review::PermissionChoice;

fn response() -> Value {
    json!({
        "schema_version": 1, "id": "rv-0123456789abcdef0123456789abcdef",
        "revision": 1, "kind": "install", "status": "pending",
        "status_message": null,
        "subject": {"app_id": "review-app", "name": "Review App", "version": "1.0.0", "publisher": null},
        "initiator": "terminal", "context": [], "permissions": [], "disclosures": [],
        "changes": null, "contract_digest": "sha256:fixture",
        "actions": ["cancel", "confirm_install"],
    })
}

fn capability_response() -> SystemReview {
    let mut value = response();
    value["id"] = json!("ap-0123456789ab");
    value["kind"] = json!("capability");
    value["actions"] = json!(["cancel", "apply_choices"]);
    value["permissions"] = json!([
        {"id":"one", "verb":"sys.observe", "label":"Observe", "blurb":"Inspect hardware",
            "risk":"low", "scope":{"kind":"fixed","description":"hardware"},
            "condition":null, "uses":[], "current":null,
            "supported_choices":["deny","allow_once"], "unsupported_reason":null},
        {"id":"two", "verb":"sys.observe", "label":"Observe", "blurb":"Inspect network",
            "risk":"low", "scope":{"kind":"fixed","description":"network"},
            "condition":null, "uses":[], "current":null,
            "supported_choices":["deny","allow_once"], "unsupported_reason":null},
    ]);
    parse_review(value).unwrap()
}

#[test]
fn system_review_terminal_rejects_malformed_grant_shaped_and_unrevisioned_views() {
    assert!(parse_review(response()).is_ok());
    let mut malformed = response();
    malformed["id"] = json!("rv-\u{1b}[2J");
    assert!(parse_review(malformed).is_err());
    let mut grant = response();
    grant["permissions_granted"] = json!(true);
    assert!(parse_review(grant).is_err());
    let mut unrevisioned = response();
    unrevisioned["revision"] = json!(0);
    assert!(parse_review(unrevisioned).is_err());
}

#[test]
fn system_review_terminal_uses_the_shared_formatter_and_independent_choices() {
    let review = capability_response();
    let text = format_terminal(&review).unwrap();
    assert!(text.contains("hardware") && text.contains("network"));
    let mut input = std::io::Cursor::new(b"2\n1\n");
    let mut output = Vec::new();
    let choices = permission_choices(&review, &mut input, &mut output).unwrap();
    assert_eq!(
        choices,
        vec![
            PermissionSelection {
                permission_id: "one".into(),
                choice: PermissionChoice::AllowOnce
            },
            PermissionSelection {
                permission_id: "two".into(),
                choice: PermissionChoice::Deny
            },
        ]
    );
    let decision = ReviewDecision {
        id: review.id.clone(),
        revision: review.revision,
        action: ReviewAction::ApplyChoices,
        choices,
    };
    decision.validate_for(&review).unwrap();
}

#[test]
fn system_review_terminal_never_defaults_an_empty_or_unsupported_permission_choice() {
    let review = capability_response();
    for input in [b"\n".as_slice(), b"0\n", b"3\n", b"yes\n"] {
        assert!(
            permission_choices(&review, &mut std::io::Cursor::new(input), &mut Vec::new()).is_err()
        );
    }
}

#[test]
fn system_review_terminal_binds_install_confirmation_to_the_staged_contract() {
    let review = parse_review(response()).unwrap();
    let mut local = PermissionReview::from_manifest(
        &crate::caps::Manifest::from_json(
            r#"{"id":"review-app","version":"1.0.0","name":{"en":"Review App"}}"#,
        )
        .unwrap(),
    )
    .unwrap();
    local.contract_digest = "sha256:fixture".into();
    let mut package = PackageRef {
        kind: crate::provenance::PackageKind::App,
        id: "review-app".into(),
        content_digest: "sha256:fixture".into(),
        tier: "user".into(),
        publisher_key_id: None,
    };
    assert!(matches_install(&review, &package, &local));
    package.id = "other".into();
    assert!(!matches_install(&review, &package, &local));
    package.id = "review-app".into();
    local.contract_digest = "sha256:changed".into();
    assert!(!matches_install(&review, &package, &local));
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
