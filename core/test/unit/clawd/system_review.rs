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
