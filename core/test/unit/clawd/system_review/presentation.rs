use super::*;
use crate::provenance::{PackageKind, VerifiedPackage, VerifyOptions};

fn isolated(test: impl FnOnce(&std::path::Path)) {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let _caps =
        crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", root.path().join("caps"));
    test(root.path());
}

fn package(root: &std::path::Path) -> VerifiedPackage {
    let source = root.join("app");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("app.json"), r#"{
        "id":"review-app","version":"1.0.0","name":{"en":"Review App"},
        "operations":{"status":{"label":{"en":"Status"},
            "args":[{"name":"domain","kind":"name","required":true,"choices":["hardware","network"]}],
            "needs":[{"verb":"sys.observe","scope":{"kind":"from-arg","arg":"domain"},
                "when":{"kind":"arg-present","arg":"domain"},
                "why":{"en":"Inspect the domain selected by the user"}}]}}
    }"#).unwrap();
    std::fs::write(source.join("main.py"), "# never executed\n").unwrap();
    crate::test_env::sign_test_package(&source, PackageKind::App, "review-app");
    crate::provenance::verify::verify_package(
        &source,
        &VerifyOptions::new(PackageKind::App),
        &crate::provenance::trust_store(),
    )
    .unwrap()
}

#[test]
fn system_review_shared_app_view_preserves_purpose_scope_arguments_and_non_grant_semantics() {
    isolated(|root| {
        let package = package(root);
        let record = approvals::system_review::submit(
            1000,
            approvals::system_review::ReviewKind::AppInstall,
            &package,
            "terminal".into(),
        )
        .unwrap();
        let view = app(&record).unwrap();
        assert_eq!(view.kind, ReviewKind::Install);
        assert_eq!(view.status, ReviewStatus::Pending);
        assert_eq!(view.permissions[0].scope.kind, ScopeKind::LateBound);
        assert!(view.permissions[0]
            .condition
            .as_ref()
            .unwrap()
            .contains("domain"));
        assert_eq!(
            view.permissions[0].uses[0].purpose,
            "Inspect the domain selected by the user"
        );
        assert!(view.permissions[0].supported_choices.is_empty());
        assert!(view.permissions[0]
            .unsupported_reason
            .as_ref()
            .unwrap()
            .contains("does not grant"));
        assert!(view
            .disclosures
            .iter()
            .any(|detail| detail.label.contains("Argument constraints")
                && detail.value.contains("network")));
        assert_eq!(
            view.actions,
            vec![ReviewAction::Cancel, ReviewAction::ConfirmInstall]
        );
        assert_eq!(app(&record).unwrap(), view);
        let confirmed =
            approvals::system_review::decide(1000, &record.id, true, Some(&package)).unwrap();
        let confirmed = app(&confirmed).unwrap();
        assert_eq!(confirmed.status, ReviewStatus::Confirmed);
        assert!(confirmed.revision > view.revision);
        assert!(confirmed.actions.is_empty());
        let consumed = approvals::system_review::consume(1000, &record.id, &package).unwrap();
        let consumed = app(&consumed).unwrap();
        assert_eq!(consumed.status, ReviewStatus::Consumed);
        assert_ne!(consumed.status, ReviewStatus::Completed);
        assert!(consumed.revision > confirmed.revision);
        assert!(approvals::list_recent(10).is_empty());
    });
}

#[test]
fn system_review_queue_merges_existing_capability_and_app_requests_with_exact_limits() {
    isolated(|root| {
        let package = package(root);
        let record = approvals::system_review::submit(
            1000,
            approvals::system_review::ReviewKind::AppActivation,
            &package,
            "desktop".into(),
        )
        .unwrap();
        let submit = |owner| {
            approvals::submit_owned(
                Verb::SYS_OBSERVE,
                crate::caps::Scope::name("hardware"),
                "terminal",
                "Inspect hardware",
                None,
                Some(owner),
            )
            .unwrap()
        };
        let capability = submit(1000);
        let foreign = submit(1001);
        let reviews = pending(1000, 100).unwrap().reviews;
        assert_eq!(reviews.len(), 2);
        assert!(reviews
            .iter()
            .any(|review| review.id == record.id && review.kind == ReviewKind::Activation));
        assert!(reviews
            .iter()
            .any(|review| review.id == capability && review.kind == ReviewKind::Capability));
        assert!(!reviews.iter().any(|review| review.id == foreign));
        assert_eq!(pending(1000, 1).unwrap().reviews.len(), 1);
        assert_eq!(pending(1000, 0).unwrap().reviews.len(), 0);
        assert!(get(1001, &record.id).is_err());
        assert!(get(1001, &capability).is_err());
    });
}

#[test]
fn system_review_stale_execution_is_visible_but_cannot_offer_choices() {
    isolated(|_| {
        let id = approvals::submit_owned(
            Verb::SYS_OBSERVE,
            crate::caps::Scope::name("hardware"),
            "expired-task",
            "Inspect hardware",
            None,
            Some(1000),
        )
        .unwrap();
        let path = crate::paths::caps_data_dir()
            .join("approvals")
            .join("pending")
            .join(format!("{id}.json"));
        let mut request = approvals::lookup_pending(&id).unwrap();
        request.context = Some(crate::caps::ConsentContext::Attended);
        std::fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
        let review = get(1000, &id).unwrap();
        assert_eq!(review.status, ReviewStatus::Stale);
        assert!(review.status_message.unwrap().contains("execution binding"));
        assert!(review.actions.is_empty());
        assert!(review.permissions[0].supported_choices.is_empty());
        assert_eq!(pending(1000, 100).unwrap().reviews.len(), 1);
    });
}

#[test]
fn system_review_attended_choices_follow_the_authoritys_actual_risk_limits() {
    isolated(|_| {
        approvals::LocalApprovalInvocation::new("typed-review-critical")
            .unwrap()
            .sync_scope(|| {
                let verb = crate::caps::ALL_VERBS
                    .iter()
                    .copied()
                    .find(|verb| catalog::lookup(*verb).unwrap().risk == Risk::Critical)
                    .unwrap();
                let id = approvals::submit_owned_with_context(
                    verb,
                    crate::caps::Scope::Wild,
                    "typed-critical",
                    "Critical operation",
                    Some("terminal".into()),
                    Some(1000),
                    Some(crate::caps::ConsentContext::Attended),
                )
                .unwrap();
                let review = get(1000, &id).unwrap();
                assert_eq!(
                    review.permissions[0].supported_choices,
                    vec![PermissionChoice::Deny, PermissionChoice::AllowOnce]
                );
                assert!(review
                    .disclosures
                    .iter()
                    .any(|detail| detail.value.contains("512 uses")));
            });
    });
}
