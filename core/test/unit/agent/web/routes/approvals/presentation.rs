use super::*;

fn package_impact_review() -> SystemReview {
    serde_json::from_value(json!({
        "schema_version":1,"id":"pg-synthetic","revision":9007199254740993_u64,
        "kind":"package_impact","status":"pending","status_message":"Stop consent is not package success.",
        "subject":{"app_id":null,"name":"All-App stop","version":null,"publisher":null},
        "initiator":"Root package transaction","context":[{"label":"Plan","value":"exact Root plan"}],
        "permissions":[],"disclosures":[{"label":"Scope","value":"All users; unsaved work may be lost."}],
        "changes":null,"contract_digest":"a".repeat(64),
        "actions":["cancel","confirm_global_app_stop"]
    })).unwrap()
}

#[test]
fn separate_package_stop_projects_root_details_and_derives_only_its_current_action() {
    let review = package_impact_review();
    let projected = project_system(review.clone()).unwrap();
    assert_eq!(projected.revision, "9007199254740993");
    assert_eq!(projected.disclosures[0].value, "All users; unsaved work may be lost.");
    let body = || SystemDecisionBody { revision: "9007199254740993".into() };
    let decision = system_decision_for(&review, body(), false).unwrap();
    assert_eq!(decision.action, ReviewAction::ConfirmGlobalAppStop);
    assert!(decision.choices.is_empty());
    assert_eq!(system_decision_for(&review, body(), true).unwrap().action, ReviewAction::Cancel);
    assert!(project(review.clone()).is_err());
    assert!(decision_for(&review, DecisionBody {
        revision: "9007199254740993".into(), choice: PermissionChoice::AllowOnce,
    }).is_err());
    let mut handled = review.clone();
    handled.status = ReviewStatus::Consumed;
    handled.actions.clear();
    assert!(system_decision_for(&handled, body(), false).is_err());
    assert_eq!(project_system(handled).unwrap().status, ReviewStatus::Consumed);
}

#[test]
fn package_stop_rejects_stale_app_capability_and_client_consent_shapes() {
    let review = package_impact_review();
    assert_eq!(system_decision_for(&review, SystemDecisionBody {
        revision: "7".into(),
    }, false).unwrap_err().0, StatusCode::CONFLICT);
    let mut denied = review.clone();
    denied.actions = vec![ReviewAction::Cancel];
    assert!(system_decision_for(&denied, SystemDecisionBody {
        revision: review.revision.to_string(),
    }, false).is_err());
    assert!(system_decision_for(&super::tests::review(), SystemDecisionBody {
        revision: "3".into(),
    }, false).is_err());
    for field in ["confirmed", "approve", "owner_uid", "action", "choices"] {
        let mut value = json!({"revision":review.revision.to_string()});
        value[field] = json!(true);
        assert!(serde_json::from_value::<SystemDecisionBody>(value).is_err(), "{field}");
    }
}

fn review() -> SystemReview {
    serde_json::from_value(json!({
        "schema_version":1, "id":"approval-1", "revision":3, "kind":"capability",
        "status":"pending", "status_message":null,
        "subject":{"app_id":null,"name":"Claw Agent","version":null,"publisher":null},
        "initiator":"Claw Agent", "context":[{"label":"Requester-supplied reason","value":"Read the requested report"}],
        "permissions":[{
            "id":"approval-1","verb":"fs.read","label":"Read files","blurb":"Read the specified files.",
            "risk":"medium","scope":{"kind":"fixed","description":"/tmp/report.txt"},
            "condition":null,"uses":[],"current":null,
            "supported_choices":["deny","allow_once"],"unsupported_reason":null
        }],
        "disclosures":[],"changes":null,"contract_digest":null,"actions":["cancel","apply_choices"]
    })).unwrap()
}

#[test]
fn web_reviews_use_os_labels_and_the_displayed_revision() {
    let view = project(review()).unwrap();
    assert_eq!(view.label, "Read files");
    assert_eq!(view.target, "/tmp/report.txt");
    assert_eq!(view.revision, "3");
    let decision = decision_for(
        &review(),
        DecisionBody {
            revision: "3".into(),
            choice: PermissionChoice::AllowOnce,
        },
    )
    .unwrap();
    assert_eq!(decision.action, ReviewAction::ApplyChoices);
    assert_eq!(decision.choices[0].permission_id, "approval-1");
    assert_eq!(
        decision_for(
            &review(),
            DecisionBody {
                revision: "2".into(),
                choice: PermissionChoice::AllowOnce,
            }
        )
        .unwrap_err()
        .0,
        StatusCode::CONFLICT
    );
}

#[test]
fn denial_grants_nothing_and_unsupported_or_forged_choices_are_rejected() {
    let denied = decision_for(
        &review(),
        DecisionBody {
            revision: "3".into(),
            choice: PermissionChoice::Deny,
        },
    )
    .unwrap();
    assert_eq!(denied.action, ReviewAction::Cancel);
    assert!(denied.choices.is_empty());
    assert!(decision_for(
        &review(),
        DecisionBody {
            revision: "3".into(),
            choice: PermissionChoice::AllowForever,
        }
    )
    .is_err());
    assert!(serde_json::from_value::<DecisionBody>(json!({
        "revision":"3", "choice":"allow_once", "owner_uid":0
    }))
    .is_err());
}
