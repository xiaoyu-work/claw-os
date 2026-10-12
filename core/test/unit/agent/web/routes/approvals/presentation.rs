use super::*;

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
