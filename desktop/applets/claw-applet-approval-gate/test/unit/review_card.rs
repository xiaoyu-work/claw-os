use super::*;
use crate::{
    review::ReviewModel,
    test_support::{capability_review, install_review, pending},
};
use clawd_client::system_review::{
    ReviewChanges, ReviewDetail, ReviewRisk, ReviewStatus, format_terminal, format_terminal_with,
};

#[test]
fn native_card_constructs_for_headless_install_capability_and_handled_states() {
    let mut model = ReviewModel::default();
    model
        .receive_pending(pending(install_review("install")))
        .unwrap();
    let element = review_card(&model.cards[0]);
    assert!(!element.as_widget().children().is_empty());
    for status in [
        ReviewStatus::Pending,
        ReviewStatus::Confirmed,
        ReviewStatus::Consumed,
        ReviewStatus::Completed,
        ReviewStatus::Stale,
        ReviewStatus::Expired,
        ReviewStatus::Cancelled,
        ReviewStatus::Failed,
    ] {
        let mut view = capability_review("capability");
        view.status = status;
        if status != ReviewStatus::Pending {
            view.actions.clear();
        }
        let mut model = ReviewModel::default();
        model.receive_pending(pending(view)).unwrap();
        let element = review_card(&model.cards[0]);
        assert!(!element.as_widget().children().is_empty());
    }
}

#[test]
fn native_card_constructs_with_busy_errors_and_disabled_unsupported_choices() {
    let mut model = ReviewModel::default();
    model
        .receive_pending(pending(install_review("install")))
        .unwrap();
    let decision = model
        .begin("install", 1, ReviewAction::ConfirmInstall)
        .unwrap();
    assert!(
        !review_card(&model.cards[0])
            .as_widget()
            .children()
            .is_empty()
    );
    model
        .decision_returned(
            "install",
            decision.revision,
            Err("synthetic cancellation".into()),
        )
        .unwrap();
    assert!(
        !review_card(&model.cards[0])
            .as_widget()
            .children()
            .is_empty()
    );
    model
        .show_failed("install", "synthetic stale error".into())
        .unwrap();
    assert!(
        !review_card(&model.cards[0])
            .as_widget()
            .children()
            .is_empty()
    );
    assert!(!model.cards[0].can_act(ReviewAction::ConfirmInstall));
}

#[test]
fn english_native_and_terminal_labels_and_decision_semantics_are_identical() {
    let mut view = capability_review("capability");
    view.context.push(ReviewDetail {
        label: "Caller".into(),
        value: "Synthetic".into(),
    });
    view.disclosures.push(ReviewDetail {
        label: "AI".into(),
        value: "Separate consent required".into(),
    });
    view.changes = Some(ReviewChanges {
        summary: "Synthetic update".into(),
        details: vec![ReviewDetail {
            label: "Added".into(),
            value: "inspect".into(),
        }],
    });
    view.permissions[0].condition = Some("When requested".into());
    view.permissions[0].scope.kind = ScopeKind::LateBound;
    view.permissions[0].supported_choices = vec![
        PermissionChoice::Deny,
        PermissionChoice::Restore,
        PermissionChoice::Ask,
        PermissionChoice::AllowOnce,
        PermissionChoice::AllowSession,
        PermissionChoice::AllowForever,
    ];
    for risk in [
        ReviewRisk::Low,
        ReviewRisk::Medium,
        ReviewRisk::High,
        ReviewRisk::Critical,
    ] {
        view.permissions[0].risk = risk;
        let native_labels = format_terminal_with(&view, review_label).unwrap();
        assert_eq!(native_labels, format_terminal(&view).unwrap());
    }
    for label in [
        ReviewText::Busy,
        ReviewText::Refresh,
        ReviewText::Close,
        ReviewText::Error,
        ReviewText::NoPending,
        ReviewText::RefreshRequired,
        ReviewText::SeparateChoices,
        ReviewText::ConfirmInstall,
        ReviewText::ConfirmActivation,
        ReviewText::ConfirmUpdate,
        ReviewText::NoGrantNotice,
        ReviewText::Confirmed,
        ReviewText::Consumed,
        ReviewText::Completed,
        ReviewText::Cancelled,
        ReviewText::Stale,
        ReviewText::Expired,
        ReviewText::Failed,
    ] {
        assert_eq!(review_label(label), label.english(), "{}", label.key());
    }
}

#[test]
fn fixed_policy_restore_does_not_enable_a_late_bound_resource_switch() {
    let mut view = capability_review("fixed-policy");
    view.permissions[0].supported_choices = vec![PermissionChoice::Deny, PermissionChoice::Restore];
    view.permissions[0].current = Some(PermissionChoice::Deny);
    let mut resource = view.permissions[0].clone();
    resource.id = "late-resource".into();
    resource.verb = "fs.read".into();
    resource.scope.kind = ScopeKind::LateBound;
    resource.scope.description = "A file selected at use".into();
    resource.supported_choices.clear();
    resource.current = None;
    resource.unsupported_reason = Some("No filesystem resource switch is supported here.".into());
    view.permissions.push(resource);
    let mut model = ReviewModel::default();
    model.receive_pending(pending(view)).unwrap();
    assert!(
        model
            .select(
                "fixed-policy",
                1,
                "late-resource",
                Some(PermissionChoice::Restore)
            )
            .is_err()
    );
    assert!(
        model
            .select(
                "fixed-policy",
                1,
                "permission-1",
                Some(PermissionChoice::Ask)
            )
            .is_err()
    );
    model
        .select(
            "fixed-policy",
            1,
            "permission-1",
            Some(PermissionChoice::Restore),
        )
        .unwrap();
    assert!(
        !review_card(&model.cards[0])
            .as_widget()
            .children()
            .is_empty()
    );
    let decision = model
        .begin("fixed-policy", 1, ReviewAction::ApplyChoices)
        .unwrap();
    assert_eq!(decision.choices.len(), 1);
    assert_eq!(decision.choices[0].choice, PermissionChoice::Restore);
    assert_eq!(review_label(ReviewText::Restore), "Restore App policy");
}
