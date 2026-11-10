use super::*;
use crate::provenance::{PackageKind, VerifiedPackage, VerifyOptions};
use serde_json::json;

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
fn system_review_named_gui_targets_require_fresh_protected_confirmation() {
    isolated(|root| {
        let source = root.join("named-gui");
        std::fs::create_dir(&source).unwrap();
        let mut manifest = json!({
            "id":"review-gui","version":"1.0.0","name":{"en":"Independent GUI"},
            "entry":"command.py","desktop":{"entry":"window.py","exec":"open","needs":[],"entries":{
                "applet":{"entry":"widget.py","exec":"panel","name":{"en":"Independent widget"},"needs":[{
                    "verb":"sys.observe","scope":{"kind":"fixed","scope":{"kind":"name","value":"audio"}},
                    "why":{"en":"Show the widget's own audio status"}
                },{
                    "verb":"fs.read","scope":{"kind":"from-arg","arg":"path"},"why":{"en":"Unbound widget file"}
                }]}
            }}
        });
        for entry in ["command.py", "window.py", "widget.py", "other.py"] {
            std::fs::write(source.join(entry), "# never executed by review\n").unwrap();
        }
        let seal = |manifest: &serde_json::Value| {
            std::fs::write(source.join("app.json"), serde_json::to_vec(manifest).unwrap()).unwrap();
            let _ = std::fs::remove_file(source.join(crate::provenance::envelope::ENVELOPE_FILE));
            crate::test_env::sign_test_package_with_entrypoints(
                &source, PackageKind::App, "review-gui", &["command.py", "window.py", "widget.py", "other.py"],
            );
            crate::provenance::verify::verify_package(
                &source, &VerifyOptions::new(PackageKind::App), &crate::provenance::trust_store(),
            ).unwrap()
        };
        let package = seal(&manifest);
        let record = approvals::system_review::submit(
            1000, approvals::system_review::ReviewKind::AppActivation, &package, "terminal".into(),
        ).unwrap();
        let view = app(&record).unwrap();
        assert!(view.disclosures.iter().any(|detail|
            detail.label.contains("applet") && detail.value.contains("widget.py")
                && detail.value.contains("Show the widget's own audio status")));
        assert!(view.disclosures.iter().any(|detail| detail.label.contains("unsupported/unbound")));
        assert!(view.permissions.iter().all(|permission| permission.supported_choices.is_empty()));
        assert!(approvals::system_review::decide(1001, &record.id, true, Some(&package)).is_err());
        approvals::system_review::decide(1000, &record.id, true, Some(&package)).unwrap();
        approvals::system_review::consume(1000, &record.id, &package).unwrap();
        assert!(approvals::system_review::has_accepted(1000, &package).unwrap());

        manifest["desktop"]["entries"]["applet"]["entry"] = json!("other.py");
        let changed = seal(&manifest);
        assert!(!approvals::system_review::has_accepted(1000, &changed).unwrap());
        let fresh = approvals::system_review::submit(
            1000, approvals::system_review::ReviewKind::AppActivation, &changed, "terminal".into(),
        ).unwrap();
        let fresh = app(&fresh).unwrap();
        assert_eq!(fresh.status, ReviewStatus::Pending);
        assert_ne!(fresh.contract_digest, view.contract_digest);
        assert!(fresh.disclosures.iter().any(|detail| detail.label.contains("applet") && detail.value.contains("other.py")));
        assert!(approvals::list_recent(10).is_empty());
    });
}

#[test]
fn protected_gui_review_resolves_owner_path_from_the_record_owner_not_request_data() {
    isolated(|root| {
        let initial = package(root);
        let source = initial.dir();
        let mut manifest: serde_json::Value = serde_json::from_str(&initial.manifest_text().unwrap()).unwrap();
        manifest["desktop"] = json!({"needs":[{
            "verb":"fs.delete","scope":{"kind":"owner-path","path":"owner-path-review/**"},
            "why":{"en":"Delete the owner's explicitly selected files"}
        }]});
        std::fs::write(source.join("app.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::remove_file(source.join(crate::provenance::envelope::ENVELOPE_FILE)).unwrap();
        crate::test_env::sign_test_package(source, PackageKind::App, "review-app");
        let verified = crate::provenance::verify::verify_package(
            source, &VerifyOptions::new(PackageKind::App), &crate::provenance::trust_store(),
        ).unwrap();
        let owner = unsafe { libc::geteuid() };
        let home = crate::paths::verified_home_for_uid(owner).unwrap();
        let record = approvals::system_review::submit(
            owner, approvals::system_review::ReviewKind::AppActivation, &verified, "terminal".into(),
        ).unwrap();
        let view = app(&record).unwrap();
        let permission = view.permissions.iter().find(|permission| permission.verb == "fs.delete").unwrap();
        assert_eq!(permission.scope.kind, ScopeKind::Fixed);
        assert_eq!(permission.scope.description, format!("path:{}/owner-path-review/**", home.display()));
        assert_eq!(permission.uses[0].purpose, "Delete the owner's explicitly selected files");
        assert!(permission.supported_choices.is_empty());
        assert_eq!(view.status, ReviewStatus::Pending);
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
