use clawd_client::system_review::*;

pub fn capability_review(id: &str) -> SystemReview {
    SystemReview {
        schema_version: SCHEMA_VERSION,
        id: id.into(),
        revision: 1,
        kind: ReviewKind::Capability,
        status: ReviewStatus::Pending,
        status_message: None,
        subject: ReviewSubject {
            app_id: Some("headless-fixture".into()),
            name: "Headless fixture".into(),
            version: Some("1.0".into()),
            publisher: Some("Synthetic publisher".into()),
        },
        initiator: "synthetic terminal session".into(),
        context: vec![],
        permissions: vec![ReviewPermission {
            id: "permission-1".into(),
            verb: "sys.fixture".into(),
            label: "Inspect fixture".into(),
            blurb: "OS-owned description".into(),
            risk: ReviewRisk::Low,
            scope: ReviewScope {
                kind: ScopeKind::Fixed,
                description: "The synthetic test fixture".into(),
            },
            condition: None,
            uses: vec![PermissionUse {
                function: "fixture.inspect".into(),
                purpose: "App-supplied intent".into(),
            }],
            current: None,
            supported_choices: vec![
                PermissionChoice::Deny,
                PermissionChoice::AllowOnce,
                PermissionChoice::AllowSession,
            ],
            unsupported_reason: Some("Persistent permission changes are not supported.".into()),
        }],
        disclosures: vec![],
        changes: None,
        contract_digest: Some("sha256:comparison-not-proof".into()),
        actions: vec![ReviewAction::Cancel, ReviewAction::ApplyChoices],
    }
}

pub fn install_review(id: &str) -> SystemReview {
    let mut review = capability_review(id);
    review.kind = ReviewKind::Install;
    review.actions = vec![ReviewAction::Cancel, ReviewAction::ConfirmInstall];
    review.permissions[0].supported_choices.clear();
    review.permissions[0].unsupported_reason =
        Some("Installation confirmation grants no capabilities.".into());
    review
}

pub fn pending(review: SystemReview) -> PendingReviews {
    PendingReviews {
        reviews: vec![review],
    }
}
