use super::*;
use crate::test_support::{capability_review, install_review, pending};

#[test]
fn defaults_leave_current_grants_unchanged_and_require_explicit_choices() {
    let mut view = capability_review("request-1");
    view.permissions[0].current = Some(PermissionChoice::AllowForever);
    let mut model = ReviewModel::default();
    model.receive_pending(pending(view)).unwrap();
    let card = &model.cards[0];
    assert_eq!(card.selected("permission-1"), None);
    assert!(!card.can_act(ReviewAction::ApplyChoices));
    assert!(card.can_act(ReviewAction::Cancel));
    assert!(
        model
            .begin("request-1", 1, ReviewAction::ApplyChoices)
            .is_err()
    );
    assert!(
        model
            .select(
                "request-1",
                1,
                "permission-1",
                Some(PermissionChoice::AllowForever)
            )
            .is_err()
    );
}

#[test]
fn legacy_capability_choices_remain_explicit_and_per_request() {
    let mut model = ReviewModel::default();
    model
        .receive_pending(PendingReviews {
            reviews: vec![capability_review("first"), capability_review("second")],
        })
        .unwrap();
    model
        .select(
            "first",
            1,
            "permission-1",
            Some(PermissionChoice::AllowSession),
        )
        .unwrap();
    let decision = model.begin("first", 1, ReviewAction::ApplyChoices).unwrap();
    assert_eq!(decision.id, "first");
    assert_eq!(
        decision.choices,
        [PermissionSelection {
            permission_id: "permission-1".into(),
            choice: PermissionChoice::AllowSession,
        }]
    );
    assert!(model.cards[0].busy());
    assert!(!model.cards[0].can_choose());
    assert!(model.begin("first", 1, ReviewAction::ApplyChoices).is_err());
    assert!(
        model
            .select("first", 1, "permission-1", Some(PermissionChoice::Deny))
            .is_err()
    );
    assert!(model.cards[1].can_choose());
}

#[test]
fn install_confirmation_never_carries_choices_and_needs_no_app_gui() {
    let mut model = ReviewModel::default();
    model
        .receive_pending(pending(install_review("install")))
        .unwrap();
    assert!(!model.cards[0].can_choose());
    assert!(
        model
            .select(
                "install",
                1,
                "permission-1",
                Some(PermissionChoice::AllowOnce)
            )
            .is_err()
    );
    let decision = model
        .begin("install", 1, ReviewAction::ConfirmInstall)
        .unwrap();
    assert!(decision.choices.is_empty());
    assert_eq!(decision.action, ReviewAction::ConfirmInstall);
}

#[test]
fn helper_reply_is_not_authority_and_refresh_prevents_duplicate_decisions() {
    let original = install_review("install");
    let mut model = ReviewModel::default();
    model.receive_pending(pending(original.clone())).unwrap();
    let decision = model
        .begin("install", 1, ReviewAction::ConfirmInstall)
        .unwrap();
    let mut handled = original.clone();
    handled.revision += 1;
    handled.status = ReviewStatus::Confirmed;
    handled.actions.clear();
    model
        .decision_returned("install", decision.revision, Ok(handled.clone()))
        .unwrap();
    assert_eq!(model.cards[0].review.status, ReviewStatus::Pending);
    assert_eq!(model.cards[0].phase, CardPhase::Refreshing);
    assert!(
        model
            .begin("install", 1, ReviewAction::ConfirmInstall)
            .is_err()
    );
    model.receive_pending(pending(original)).unwrap();
    assert_eq!(model.cards[0].phase, CardPhase::Refreshing);
    model.receive_show(handled).unwrap();
    assert_eq!(model.cards[0].review.status, ReviewStatus::Confirmed);
    assert!(!model.cards[0].can_act(ReviewAction::ConfirmInstall));
}

#[test]
fn owner_cancel_never_submits_choices_and_always_requires_broker_refresh() {
    let original = capability_review("cancel");
    let mut cancelled = original.clone();
    cancelled.revision += 1;
    cancelled.status = ReviewStatus::Cancelled;
    cancelled.actions.clear();
    for result in [
        Ok(cancelled.clone()),
        Err("synthetic transport failure".into()),
    ] {
        let mut model = ReviewModel::default();
        model.receive_pending(pending(original.clone())).unwrap();
        model
            .select("cancel", 1, "permission-1", Some(PermissionChoice::Deny))
            .unwrap();
        let decision = model.begin("cancel", 1, ReviewAction::Cancel).unwrap();
        assert_eq!(decision.revision, 1);
        assert!(decision.choices.is_empty());
        model
            .decision_returned("cancel", decision.revision, result)
            .unwrap();
        assert_eq!(model.cards[0].review, original);
        assert_eq!(model.cards[0].phase, CardPhase::Refreshing);
        assert!(model.begin("cancel", 1, ReviewAction::Cancel).is_err());
        model.receive_pending(pending(original.clone())).unwrap();
        assert_eq!(model.cards[0].phase, CardPhase::Refreshing);
        model.receive_show(cancelled.clone()).unwrap();
        assert_eq!(model.cards[0].review.status, ReviewStatus::Cancelled);
        assert_eq!(model.cards[0].selected("permission-1"), None);
        assert!(!model.cards[0].can_act(ReviewAction::Cancel));
    }
}

#[test]
fn disappeared_requests_are_refreshed_not_assumed_approved() {
    let mut model = ReviewModel::default();
    model
        .receive_pending(pending(install_review("install")))
        .unwrap();
    let ids = model
        .receive_pending(PendingReviews { reviews: vec![] })
        .unwrap();
    assert_eq!(ids, ["install"]);
    assert_eq!(model.cards[0].phase, CardPhase::Refreshing);
    assert_eq!(model.cards[0].review.status, ReviewStatus::Pending);
    assert!(
        model
            .receive_pending(PendingReviews { reviews: vec![] })
            .unwrap()
            .is_empty()
    );
    model
        .show_failed("install", "request unavailable".into())
        .unwrap();
    assert!(!model.cards[0].can_act(ReviewAction::ConfirmInstall));
    assert!(
        model.cards[0]
            .notice
            .as_ref()
            .unwrap()
            .contains("unavailable")
    );
}

#[test]
fn revision_changes_clear_drafts_and_old_poll_results_cannot_reopen_handled_requests() {
    let original = capability_review("capability");
    let mut model = ReviewModel::default();
    model.receive_pending(pending(original.clone())).unwrap();
    model
        .select(
            "capability",
            1,
            "permission-1",
            Some(PermissionChoice::AllowOnce),
        )
        .unwrap();
    let mut changed = original.clone();
    changed.revision += 1;
    changed.permissions[0].supported_choices = vec![PermissionChoice::Deny];
    model.receive_pending(pending(changed.clone())).unwrap();
    assert_eq!(model.cards[0].selected("permission-1"), None);
    changed.revision += 1;
    changed.status = ReviewStatus::Completed;
    changed.actions.clear();
    model.receive_show(changed).unwrap();
    model.receive_pending(pending(original)).unwrap();
    assert_eq!(model.cards[0].review.status, ReviewStatus::Completed);
    assert!(!model.cards[0].can_choose());
}

#[test]
fn unsupported_or_inconsistent_server_data_fails_without_partial_publication() {
    let original = capability_review("capability");
    let mut model = ReviewModel::default();
    model.receive_pending(pending(original.clone())).unwrap();
    let mut bad = original;
    bad.permissions[0].scope.description = "different scope at the same revision".into();
    assert!(model.receive_pending(pending(bad)).is_err());
    assert_eq!(
        model.cards[0].review.permissions[0].scope.description,
        "The synthetic test fixture"
    );
    model.load_failed("malformed reply".into());
    assert!(!model.cards[0].can_choose());
    assert!(model.begin("capability", 1, ReviewAction::Cancel).is_err());
    model
        .receive_pending(pending(capability_review("capability")))
        .unwrap();
    assert!(model.cards[0].can_choose());
}

#[test]
fn stale_expired_completed_and_cancelled_states_have_no_active_decisions() {
    for status in [
        ReviewStatus::Stale,
        ReviewStatus::Expired,
        ReviewStatus::Completed,
        ReviewStatus::Cancelled,
        ReviewStatus::Failed,
        ReviewStatus::Confirmed,
    ] {
        let mut view = install_review("install");
        view.status = status;
        view.actions.clear();
        let mut model = ReviewModel::default();
        model.receive_pending(pending(view)).unwrap();
        assert!(!model.cards[0].can_act(ReviewAction::ConfirmInstall));
        assert!(!model.cards[0].can_act(ReviewAction::Cancel));
        assert_eq!(model.pending_count(), 0);
    }
}

#[test]
fn a_failed_decision_is_refreshed_without_retrying_or_dropping_its_diagnostic() {
    let original = install_review("install");
    let mut model = ReviewModel::default();
    model.receive_pending(pending(original.clone())).unwrap();
    let decision = model
        .begin("install", 1, ReviewAction::ConfirmInstall)
        .unwrap();
    model
        .decision_returned(
            "install",
            decision.revision,
            Err("authentication cancelled".into()),
        )
        .unwrap();
    assert_eq!(model.cards[0].phase, CardPhase::Refreshing);
    assert!(
        model
            .decision_returned("install", decision.revision, Ok(original.clone()))
            .is_err()
    );
    model.receive_show(original).unwrap();
    assert_eq!(model.cards[0].phase, CardPhase::Ready);
    assert_eq!(
        model.cards[0].notice.as_deref(),
        Some("authentication cancelled")
    );
}

#[test]
fn closing_is_local_cancellation_of_drafts_not_an_approval_or_denial() {
    let mut model = ReviewModel::default();
    model
        .receive_pending(pending(capability_review("capability")))
        .unwrap();
    model
        .select(
            "capability",
            1,
            "permission-1",
            Some(PermissionChoice::AllowOnce),
        )
        .unwrap();
    model.closed();
    assert_eq!(model.cards[0].selected("permission-1"), None);
    assert_eq!(model.cards[0].review.status, ReviewStatus::Pending);
    assert_eq!(model.cards[0].review.revision, 1);
}

#[test]
fn unknown_ids_and_unsupported_action_events_are_explicit_errors() {
    let mut model = ReviewModel::default();
    model
        .receive_pending(pending(capability_review("capability")))
        .unwrap();
    assert!(model.select("unknown", 1, "permission-1", None).is_err());
    assert!(
        model
            .select("capability", 1, "unknown", Some(PermissionChoice::Deny))
            .is_err()
    );
    assert!(
        model
            .begin("capability", 1, ReviewAction::ConfirmInstall)
            .is_err()
    );
    assert!(model.request_refresh("unknown").is_err());
}

#[test]
fn queued_clicks_are_bound_to_the_revision_that_was_actually_displayed() {
    let original = capability_review("capability");
    let mut model = ReviewModel::default();
    model.receive_pending(pending(original.clone())).unwrap();
    let mut changed = original;
    changed.revision = 2;
    changed.permissions[0].scope.description = "A different reviewed resource".into();
    model.receive_pending(pending(changed)).unwrap();
    assert!(
        model
            .select(
                "capability",
                1,
                "permission-1",
                Some(PermissionChoice::AllowOnce)
            )
            .is_err()
    );
    assert!(model.begin("capability", 1, ReviewAction::Cancel).is_err());
    assert_eq!(model.cards[0].selected("permission-1"), None);
    assert_eq!(model.cards[0].phase, CardPhase::Ready);
    model
        .select(
            "capability",
            2,
            "permission-1",
            Some(PermissionChoice::AllowOnce),
        )
        .unwrap();
    let decision = model
        .begin("capability", 2, ReviewAction::ApplyChoices)
        .unwrap();
    assert_eq!(decision.revision, 2);
}
