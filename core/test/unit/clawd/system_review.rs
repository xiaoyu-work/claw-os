use super::*;

#[test]
fn system_review_first_use_requires_the_actual_owners_confirmation() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let _caps =
        crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", root.path().join("caps"));
    let source = root.path().join("app");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(
        source.join("app.json"),
        r#"{"id":"first-use","version":"1.0.0","name":{"en":"First Use"}}"#,
    )
    .unwrap();
    std::fs::write(source.join("main.py"), "# never executed by review\n").unwrap();
    crate::test_env::sign_test_package(&source, PackageKind::App, "first-use");
    let package = crate::provenance::verify::verify_package(
        &source,
        &VerifyOptions::new(PackageKind::App),
        &crate::provenance::trust_store(),
    )
    .unwrap();
    let manifest = crate::caps::Manifest::from_json(&package.manifest_text().unwrap()).unwrap();
    let app = crate::apps::App {
        manifest,
        dir: source,
        provenance: Ok(std::sync::Arc::new(package)),
    };
    let error = require_app_review(1000, &app, "terminal").unwrap_err();
    assert_eq!(error.audit_class, Some("system_review_required"));
    let id = error.data.unwrap()["system_review_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(require_app_review(1000, &app, "terminal").is_err());
    store::decide(1000, &id, true, Some(app.require_verified().unwrap())).unwrap();
    require_app_review(1000, &app, "terminal").unwrap();
    require_app_review(1000, &app, "terminal").unwrap();
    assert!(require_app_review(1001, &app, "another user").is_err());
}

#[tokio::test]
async fn system_review_non_root_peer_cannot_confirm_even_with_forged_owner_and_state() {
    let client = ClientIdentity {
        uid: Some(1000),
        ..ClientIdentity::unknown()
    };
    let error = decide(
        json!({
            "id": "rv-0123456789abcdef0123456789abcdef",
            "owner_uid": 0,
            "decision": "approve",
            "approved": true,
        }),
        &client,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.kind,
        super::super::protocol::BrokerErrorKind::Unauthorized
    );
    assert!(error.message.contains("privileged OS approval helper"));
}

#[tokio::test]
async fn system_review_unknown_peer_cannot_read_pending_requests() {
    let error = pending(json!({"limit": 100}), &ClientIdentity::unknown())
        .await
        .unwrap_err();
    assert_eq!(
        error.kind,
        super::super::protocol::BrokerErrorKind::Unauthorized
    );
}

#[tokio::test]
async fn system_review_list_limit_refuses_zero_and_unbounded_requests() {
    let client = ClientIdentity {
        uid: Some(1000),
        ..ClientIdentity::unknown()
    };
    for limit in [0, 101, u64::MAX] {
        let error = pending(json!({"limit": limit}), &client).await.unwrap_err();
        assert!(error.message.contains("1..100"), "{}", error.message);
    }
}

fn isolated_review(test: impl FnOnce()) {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let _caps =
        crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", root.path().join("caps"));
    let _proc =
        crate::test_env::TestEnvVarGuard::set("COS_PROC_DATA_DIR", root.path().join("proc"));
    test();
}

fn capability_decision(owner: u32) -> ReviewDecision {
    let id = approvals::submit_owned(
        crate::caps::Verb::SYS_OBSERVE,
        crate::caps::Scope::name("hardware"),
        "typed-review",
        "Inspect hardware",
        Some("terminal".into()),
        Some(owner),
    )
    .unwrap();
    let review = presentation::get(owner, &id).unwrap();
    ReviewDecision {
        id: id.clone(),
        revision: review.revision,
        action: ReviewAction::ApplyChoices,
        choices: vec![clawd_client::system_review::PermissionSelection {
            permission_id: id,
            choice: PermissionChoice::AllowOnce,
        }],
    }
}

#[test]
fn system_review_typed_decision_rejects_stale_owner_and_unsupported_choices_without_granting() {
    isolated_review(|| {
        let decision = capability_decision(1000);
        assert!(apply_decision(1001, &decision).is_err());
        let mut stale = decision.clone();
        stale.revision += 1;
        assert!(apply_decision(1000, &stale)
            .unwrap_err()
            .contains("stale revision"));
        for choice in [PermissionChoice::Ask, PermissionChoice::Restore] {
            let mut unsupported = decision.clone();
            unsupported.choices[0].choice = choice;
            assert!(apply_decision(1000, &unsupported)
                .unwrap_err()
                .contains("not supported"));
        }
        let mut duplicated = decision.clone();
        duplicated.choices.push(duplicated.choices[0].clone());
        assert!(apply_decision(1000, &duplicated)
            .unwrap_err()
            .contains("duplicate"));
        let mut installation = decision.clone();
        installation.action = ReviewAction::ConfirmInstall;
        installation.choices.clear();
        assert!(apply_decision(1000, &installation).is_err());
        assert!(approvals::list_recent(100).is_empty());
        assert_eq!(approvals::list_pending_for_owner(Some(1000)).len(), 1);
    });
}

#[test]
fn system_review_typed_capability_uses_existing_one_shot_authority_and_only_one_decision_wins() {
    isolated_review(|| {
        let decision = capability_decision(1000);
        let result = apply_decision(1000, &decision).unwrap();
        assert_eq!(
            result.status,
            clawd_client::system_review::ReviewStatus::Completed
        );
        assert!(result.revision > decision.revision);
        assert!(apply_decision(1000, &decision).is_err());
        let resolved = approvals::list_recent_for_owner(10, Some(1000));
        assert_eq!(resolved.len(), 1);
        let grant = resolved[0].decision.grant.as_ref().unwrap();
        assert_eq!(grant.uses_remaining, 1);
        assert!(resolved[0].decision.restoration.is_none());
        assert_eq!(
            approvals::consume_matching_grant_for_owner(
                "typed-review",
                crate::caps::Verb::SYS_OBSERVE,
                &crate::caps::Scope::name("hardware"),
                Some(1000),
            )
            .unwrap(),
            Some(GrantDuration::Once)
        );
        assert_eq!(
            approvals::consume_matching_grant_for_owner(
                "typed-review",
                crate::caps::Verb::SYS_OBSERVE,
                &crate::caps::Scope::name("hardware"),
                Some(1000),
            )
            .unwrap(),
            None
        );
    });
}

#[test]
fn system_review_root_route_and_owner_cancellation_share_typed_state_without_self_approval() {
    isolated_review(|| {
        let uid = unsafe { libc::geteuid() };
        let mut decision = capability_decision(uid);
        let owner = ClientIdentity {
            uid: Some(uid),
            gid: Some(unsafe { libc::getegid() }),
            pid: Some(std::process::id()),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            attended_local: true,
            ..ClientIdentity::unknown()
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let forged = cancel(json!({"review":decision}), &owner)
                .await
                .unwrap_err();
            assert!(forged.message.contains("cannot grant permissions"));
            decision.action = ReviewAction::Cancel;
            decision.choices.clear();
            let mut stale = decision.clone();
            stale.revision += 1;
            assert!(cancel(json!({"review":stale}), &owner).await.is_err());
            let cancelled = cancel(json!({"review":decision}), &owner).await.unwrap();
            assert_eq!(cancelled["status"], "cancelled");
            assert!(cancel(json!({"review":decision}), &owner).await.is_err());
        });
        assert!(approvals::list_recent_for_owner(10, Some(uid))[0]
            .decision
            .grant
            .is_none());
    });
}

#[test]
fn system_review_typed_root_helper_decision_returns_the_shared_projection() {
    isolated_review(|| {
        let decision = capability_decision(1000);
        let root = ClientIdentity {
            uid: Some(0),
            ..ClientIdentity::unknown()
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime
            .block_on(decide(json!({"owner_uid":1000, "review":decision}), &root))
            .unwrap();
        let review: SystemReview = serde_json::from_value(result).unwrap();
        review.validate().unwrap();
        assert_eq!(review.id, decision.id);
        assert_eq!(
            review.status,
            clawd_client::system_review::ReviewStatus::Completed
        );
    });
}
