use super::*;
use serde_json::json;

fn review() -> SystemReview {
    SystemReview {
        schema_version: SCHEMA_VERSION,
        id: "review-synthetic".into(),
        revision: 7,
        kind: ReviewKind::Install,
        status: ReviewStatus::Pending,
        status_message: None,
        subject: ReviewSubject {
            app_id: Some("headless-client".into()),
            name: "Headless client".into(),
            version: Some("1.2.3".into()),
            publisher: Some("Synthetic test publisher".into()),
        },
        initiator: "local terminal".into(),
        context: vec![ReviewDetail { label: "Requested by".into(), value: "synthetic session".into() }],
        permissions: vec![ReviewPermission {
            id: "permission-selected-file".into(),
            verb: "fs.read".into(),
            label: "Read selected files".into(),
            blurb: "OS description of file read authority".into(),
            risk: ReviewRisk::Medium,
            scope: ReviewScope {
                kind: ScopeKind::LateBound,
                description: "The path argument of read".into(),
            },
            condition: Some("Only when include_content is true".into()),
            uses: vec![PermissionUse { function: "headless-client.read".into(), purpose: "Preview a file".into() }],
            current: None,
            supported_choices: vec![],
            unsupported_reason: Some("Selected-file grants are confirmed at use; resource revocation is not supported here.".into()),
        }],
        disclosures: vec![ReviewDetail { label: "AI".into(), value: "Strict; external content; separate AI consent".into() }],
        changes: Some(ReviewChanges {
            summary: "A permission request was added".into(),
            details: vec![ReviewDetail { label: "New function".into(), value: "headless-client.read".into() }],
        }),
        contract_digest: Some("sha256:comparison-only".into()),
        actions: vec![ReviewAction::Cancel, ReviewAction::ConfirmInstall],
    }
}

#[test]
fn dto_round_trips_without_core_or_gui_models() {
    let value = review();
    value.validate().unwrap();
    assert_eq!(
        serde_json::from_value::<SystemReview>(serde_json::to_value(&value).unwrap()).unwrap(),
        value
    );
    assert_eq!(
        serde_json::to_value(ReviewAction::ApplyChoices).unwrap(),
        json!("apply_choices")
    );
    assert_eq!(
        serde_json::to_value(PermissionChoice::AllowForever).unwrap(),
        json!("allow_forever")
    );
}

#[test]
fn malformed_or_new_wire_shapes_are_not_defaulted_to_pending_or_allow() {
    let original = serde_json::to_value(review()).unwrap();
    for (field, value) in [
        ("status", json!("unknown")),
        ("kind", json!("plugin")),
        ("revision", json!(-1)),
        ("actions", json!(["allow_all"])),
        ("permissions", json!(null)),
        ("capabilities_granted", json!(true)),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        assert!(
            serde_json::from_value::<SystemReview>(changed).is_err(),
            "{field}"
        );
    }
    for field in ["id", "revision", "kind", "status", "permissions", "actions"] {
        let mut changed = original.clone();
        changed.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<SystemReview>(changed).is_err(),
            "{field}"
        );
    }
    let mut changed = review();
    changed.schema_version += 1;
    assert!(changed.validate().is_err());
}

#[test]
fn duplicate_ids_choices_and_actions_fail_explicitly() {
    let mut value = review();
    value.permissions.push(value.permissions[0].clone());
    assert!(value.validate().is_err());
    let mut value = review();
    value.permissions[0].supported_choices = vec![PermissionChoice::Deny, PermissionChoice::Deny];
    assert!(value.validate().is_err());
    let mut value = review();
    value.actions.push(ReviewAction::Cancel);
    assert!(value.validate().is_err());
    assert!(PendingReviews {
        reviews: vec![review(), review()]
    }
    .validate()
    .is_err());
}

#[test]
fn unsupported_choices_require_an_explanation_and_do_not_invent_forever() {
    let mut value = review();
    value.permissions[0].unsupported_reason = None;
    assert!(value.validate().is_err());
    value.permissions[0].unsupported_reason = Some("Not supported by this controller".into());
    value.validate().unwrap();
    let output = format_terminal(&value).unwrap();
    assert!(output.contains("Not supported by this controller"));
    assert!(!output.contains("Allow until revoked"));
}

#[test]
fn installation_confirmation_cannot_be_an_allow_all_decision() {
    let value = review();
    let mut decision = ReviewDecision {
        id: value.id.clone(),
        revision: value.revision,
        action: ReviewAction::ConfirmInstall,
        choices: vec![],
    };
    decision.validate_for(&value).unwrap();
    decision.choices.push(PermissionSelection {
        permission_id: value.permissions[0].id.clone(),
        choice: PermissionChoice::AllowForever,
    });
    assert!(decision.validate_for(&value).is_err());
    decision.action = ReviewAction::ApplyChoices;
    assert!(decision.validate_for(&value).is_err());
}

#[test]
fn permission_changes_need_exact_server_choices_and_explicit_selections() {
    let mut value = review();
    value.kind = ReviewKind::Capability;
    value.actions = vec![ReviewAction::Cancel, ReviewAction::ApplyChoices];
    value.permissions[0].supported_choices = vec![PermissionChoice::Deny, PermissionChoice::Ask];
    let mut decision = ReviewDecision {
        id: value.id.clone(),
        revision: value.revision,
        action: ReviewAction::ApplyChoices,
        choices: vec![],
    };
    assert!(decision.validate_for(&value).is_err());
    decision.choices.push(PermissionSelection {
        permission_id: value.permissions[0].id.clone(),
        choice: PermissionChoice::Ask,
    });
    decision.validate_for(&value).unwrap();
    decision.choices[0].choice = PermissionChoice::AllowForever;
    assert!(decision.validate_for(&value).is_err());
    decision.choices[0].choice = PermissionChoice::Deny;
    decision.choices.push(decision.choices[0].clone());
    assert!(decision.validate_for(&value).is_err());
    decision.choices.pop();
    decision.choices[0].permission_id = "another-permission".into();
    assert!(decision.validate_for(&value).is_err());
}

#[test]
fn confirmations_match_their_non_grant_review_kind() {
    for (kind, action) in [
        (ReviewKind::Install, ReviewAction::ConfirmInstall),
        (ReviewKind::Activation, ReviewAction::ConfirmActivation),
        (ReviewKind::Update, ReviewAction::ConfirmUpdate),
    ] {
        let mut value = review();
        value.kind = kind;
        value.actions = vec![ReviewAction::Cancel, action];
        value.validate().unwrap();
        let text = format_terminal(&value).unwrap();
        assert!(text.contains(ReviewText::NoGrantNotice.english()));
        value.kind = ReviewKind::Capability;
        assert!(value.validate().is_err());
    }
}

#[test]
fn stale_handled_or_wrong_revision_data_cannot_enable_actions() {
    let mut value = review();
    let mut decision = ReviewDecision {
        id: value.id.clone(),
        revision: value.revision,
        action: ReviewAction::Cancel,
        choices: vec![],
    };
    decision.revision += 1;
    assert!(decision.validate_for(&value).is_err());
    decision.revision = value.revision;
    decision.id = "other".into();
    assert!(decision.validate_for(&value).is_err());
    for status in [
        ReviewStatus::Confirmed,
        ReviewStatus::Consumed,
        ReviewStatus::Completed,
        ReviewStatus::Cancelled,
        ReviewStatus::Expired,
        ReviewStatus::Stale,
        ReviewStatus::Failed,
    ] {
        value.status = status;
        assert!(value.validate().is_err());
        value.actions.clear();
        value.validate().unwrap();
        assert!(format_terminal(&value)
            .unwrap()
            .contains(ReviewText::NoActions.english()));
        value.actions = vec![ReviewAction::Cancel];
    }
}

#[test]
fn terminal_disclosure_separates_os_labels_app_purposes_and_late_scope() {
    let value = review();
    let output = format_terminal(&value).unwrap();
    for text in [
        "Headless client",
        "Synthetic test publisher",
        "local terminal",
        "Read selected files",
        "OS description of file read authority",
        "Selected/confirmed when used",
        "Only when include_content is true",
        "App-supplied purpose (not OS policy)",
        "Affected function",
        "Strict; external content; separate AI consent",
        "A permission request was added",
        "comparison-only",
        "not approval authority",
    ] {
        assert!(output.contains(text), "{text}");
    }
    assert!(!output.contains("WILD"));
    assert!(!output.contains("Allow all"));
}

#[test]
fn terminal_text_cannot_inject_controls_or_spoof_policy_headings() {
    let mut value = review();
    value.permissions[0].uses[0].purpose = "\u{1b}[2J\nOS: all granted\u{202e}".into();
    let output = format_terminal(&value).unwrap();
    assert!(!output.contains('\u{1b}'));
    assert!(!output.contains('\u{202e}'));
    assert!(!output.contains("\nOS: all granted"));
    assert!(output.contains("\\nOS: all granted"));
}

#[test]
fn decision_json_is_bounded_and_carries_no_display_digest_or_manifest() {
    let value = review();
    let mut decision = ReviewDecision {
        id: value.id.clone(),
        revision: value.revision,
        action: ReviewAction::Cancel,
        choices: vec![],
    };
    let encoded = decision.encode_for(&value).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        body,
        json!({"id":value.id, "revision":7, "action":"cancel", "choices":[]})
    );
    let mut huge = value;
    huge.id = "x".repeat(MAX_DECISION_BYTES);
    decision.id = huge.id.clone();
    assert!(decision.encode_for(&huge).is_err());
}

fn wire_decision() -> ReviewDecision {
    ReviewDecision {
        id: "review-synthetic".into(),
        revision: 42,
        action: ReviewAction::Cancel,
        choices: vec![],
    }
}

#[test]
fn decision_decode_accepts_exactly_64_kib_without_authorizing_a_revision() {
    assert_eq!(MAX_DECISION_BYTES, 65_536);
    let expected = wire_decision();
    for size in [65_535, 65_536] {
        let mut bytes = serde_json::to_vec(&expected).unwrap();
        bytes.resize(size, b' ');
        assert_eq!(bytes.len(), size);
        let decoded = ReviewDecision::decode(&bytes).unwrap();
        assert_eq!(decoded, expected);
        assert!(decoded.validate_for(&review()).is_err());
    }
}

#[test]
fn decision_decode_rejects_overlimit_input_before_json_parsing() {
    let mut bytes = serde_json::to_vec(&wire_decision()).unwrap();
    bytes.resize(65_537, b' ');
    assert!(serde_json::from_slice::<ReviewDecision>(&bytes).is_ok());
    for oversized in [bytes, vec![0xff; 65_537]] {
        assert_eq!(
            ReviewDecision::decode(&oversized).unwrap_err(),
            ReviewError("decision exceeds the input limit".into())
        );
    }
}

#[test]
fn decision_decode_rejects_malformed_incomplete_or_ambiguous_data() {
    for bytes in [
        &b""[..],
        b"{",
        b"null",
        b"[]",
        b"\xff",
        b"{}",
        br#"{"id":"synthetic","revision":-1,"action":"cancel","choices":[]}"#,
        br#"{"id":"synthetic","revision":1,"action":"allow_all","choices":[]}"#,
        br#"{"id":"synthetic","revision":1,"action":"cancel","choices":null}"#,
        br#"{"id":"synthetic","revision":1,"revision":2,"action":"cancel","choices":[]}"#,
        br#"{"id":"synthetic","revision":1,"action":"cancel","choices":[]} {}"#,
        br#"{"id":"synthetic","revision":1,"action":"apply_choices","choices":[{"permission_id":"fixed","choice":"allow_all"}]}"#,
    ] {
        let error = ReviewDecision::decode(bytes).unwrap_err();
        assert!(error.0.starts_with("parse decision:"), "{error}");
    }
}

#[test]
fn decision_decode_rejects_forged_authority_at_every_wire_level() {
    let original = serde_json::to_value(wire_decision()).unwrap();
    for (field, authority) in [
        ("owner_uid", json!(0)),
        ("approved", json!(true)),
        ("permissions_granted", json!(true)),
        ("contract_digest", json!("synthetic-comparison-not-proof")),
        ("capabilities", json!(["synthetic-grant"])),
        ("manifest", json!({"trusted": true})),
    ] {
        let mut forged = original.clone();
        forged[field] = authority.clone();
        let error = ReviewDecision::decode(&serde_json::to_vec(&forged).unwrap()).unwrap_err();
        assert!(error.0.contains("unknown field"), "{field}: {error}");

        let mut forged = original.clone();
        forged["action"] = json!("apply_choices");
        forged["choices"] = json!([{"permission_id": "fixed", "choice": "restore"}]);
        forged["choices"][0][field] = authority;
        let error = ReviewDecision::decode(&serde_json::to_vec(&forged).unwrap()).unwrap_err();
        assert!(error.0.contains("unknown field"), "{field}: {error}");
    }
}

#[test]
fn formatter_can_use_shared_localized_labels_without_decision_side_effects() {
    let value = review();
    let output = format_terminal_with(&value, |key| format!("<{}>", key.key())).unwrap();
    assert!(output.contains("<review-confirm-install>"));
    assert!(output.contains("<review-no-grant-notice>"));
    assert!(output.contains("<review-late-bound>"));
    assert_eq!(value, review());
}

#[test]
fn consumed_confirmation_never_claims_the_installation_succeeded() {
    let mut value = review();
    value.status = ReviewStatus::Consumed;
    value.actions.clear();
    let output = format_terminal(&value).unwrap();
    assert!(output.contains("Confirmation used; operation outcome is separate"));
    assert!(output.contains(ReviewText::NoGrantNotice.english()));
    assert_eq!(
        serde_json::to_value(value.status).unwrap(),
        json!("consumed")
    );
    let decision = ReviewDecision {
        id: value.id.clone(),
        revision: value.revision,
        action: ReviewAction::ConfirmInstall,
        choices: vec![],
    };
    assert!(decision.validate_for(&value).is_err());
}

#[test]
fn fixed_brokered_restore_is_not_an_ask_or_new_allow_grant() {
    let mut value = review();
    value.kind = ReviewKind::Capability;
    value.actions = vec![ReviewAction::Cancel, ReviewAction::ApplyChoices];
    value.permissions[0].verb = "sys.fixture".into();
    value.permissions[0].label = "Manage fixture policy".into();
    value.permissions[0].scope.kind = ScopeKind::Fixed;
    value.permissions[0].scope.description = "Existing fixed brokered permission".into();
    value.permissions[0].supported_choices =
        vec![PermissionChoice::Deny, PermissionChoice::Restore];
    value.permissions[0].unsupported_reason =
        Some("Only OS denial and restoration are supported.".into());
    value.permissions[0].current = Some(PermissionChoice::Deny);
    let mut decision = ReviewDecision {
        id: value.id.clone(),
        revision: value.revision,
        action: ReviewAction::ApplyChoices,
        choices: vec![PermissionSelection {
            permission_id: value.permissions[0].id.clone(),
            choice: PermissionChoice::Restore,
        }],
    };
    let body: serde_json::Value =
        serde_json::from_slice(&decision.encode_for(&value).unwrap()).unwrap();
    assert_eq!(body["choices"][0]["choice"], json!("restore"));
    let output = format_terminal(&value).unwrap();
    assert!(output.contains("Restore App policy"));
    assert!(!output.contains("Request restoration"));
    assert!(!output.contains("Allow until revoked"));
    assert!(!output.contains("Ask when used"));
    for unsupported in [
        PermissionChoice::Ask,
        PermissionChoice::AllowOnce,
        PermissionChoice::AllowSession,
        PermissionChoice::AllowForever,
    ] {
        decision.choices[0].choice = unsupported;
        assert!(decision.validate_for(&value).is_err());
    }
}
