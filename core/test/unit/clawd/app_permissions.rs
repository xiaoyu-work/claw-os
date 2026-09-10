use super::*;
use crate::test_env::TestEnvVarGuard;

fn client(uid: u32) -> ClientIdentity {
    ClientIdentity {
        uid: Some(uid),
        gid: Some(unsafe { libc::getegid() }),
        pid: Some(std::process::id()),
        execution_uid: None,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        attended_local: true,
        extension_host: None,
    }
}

#[tokio::test]
async fn verified_app_request_trusted_approval_and_revocation_share_owner_policy() {
    let _lock = crate::test_env::lock_env();
    let dir = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", dir.path().join("data"));
    let _caps = TestEnvVarGuard::set("COS_CAPS_DATA_DIR", dir.path().join("caps"));
    let _proc = TestEnvVarGuard::set("COS_PROC_DATA_DIR", dir.path().join("proc"));
    let apps = dir.path().join("apps");
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", &apps);
    let app = apps.join("permission-fixture");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("app.json"), r#"{
        "id":"permission-fixture","version":"0.0.0-test","name":{"en":"Fixture"},
        "operations":{"status":{"label":{"en":"Status"},"args":[],"needs":[
            {"verb":"sys.observe","scope":{"kind":"fixed","scope":{"kind":"name","value":"audio"}},"why":{"en":"Audio status"}}
        ]}}
    }"#).unwrap();
    std::fs::write(app.join("main.py"), "print('fixture')").unwrap();
    let uid = unsafe { libc::geteuid() };
    let trust = dir.path().join("publishers");
    std::fs::create_dir_all(&trust).unwrap();
    let key = crate::test_env::test_signing_key();
    let trust_file = trust.join("test.json");
    std::fs::write(
        &trust_file,
        serde_json::to_vec(&key.trust_entry(&[crate::provenance::PackageKind::App])).unwrap(),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&trust_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let roots = vec![crate::provenance::trust::TrustRootSpec {
        path: trust,
        tier: crate::provenance::TrustTier::User,
        allowed_uids: vec![uid],
        domain: crate::provenance::state::TrustDomain::Owner(uid),
    }];
    crate::test_env::record_trust_state(&roots);
    let trust_store = crate::provenance::TrustStore::load_roots(&roots);
    assert!(!trust_store.is_empty(), "fixture publisher did not load: {:?}", trust_store.diagnostics());
    crate::provenance::set_trust_store_for_roots(
        trust_store,
        roots,
    );
    struct ResetTrust;
    impl Drop for ResetTrust {
        fn drop(&mut self) {
            let roots = crate::provenance::TrustStore::default_roots();
            crate::provenance::set_trust_store_for_roots(
                crate::provenance::TrustStore::load_roots(&roots),
                roots,
            );
        }
    }
    let _reset = ResetTrust;
    crate::provenance::sign::sign_directory(
        &app,
        &crate::provenance::sign::SignRequest {
            kind: crate::provenance::PackageKind::App,
            id: "permission-fixture".into(),
            version: "0.0.0-test".into(),
            manifest_schema: "test".into(),
            manifest_path: "app.json".into(),
            entrypoints: vec!["main.py".into()],
            resources: vec![],
        },
        key,
    )
    .unwrap();
    let owner = client(uid);
    let query = json!({"action":"show","app_id":"permission-fixture"});
    let initial = control(query.clone(), &owner, None).await.unwrap();
    let permission = initial["permissions"][0]["permission_id"].as_str().unwrap();
    assert_eq!(initial["permissions"][0]["enabled"], true);
    assert_eq!(initial["permissions"][0]["live_granted"], false);
    let revoke =
        json!({"action":"revoke","app_id":"permission-fixture","permission_id":permission});
    assert_eq!(
        control(revoke.clone(), &owner, None).await.unwrap()["revoked"],
        true
    );
    assert_eq!(
        control(query.clone(), &owner, None).await.unwrap()["permissions"][0]["enabled"],
        false
    );
    let request = json!({"action":"request","app_id":"permission-fixture","permission_id":permission,"reason":"restore status"});
    let pending = control(request.clone(), &owner, None).await.unwrap();
    assert_eq!(pending["status"], "pending");
    assert_eq!(
        control(request, &owner, None).await.unwrap()["id"],
        pending["id"]
    );
    let decision =
        json!({"id":pending["id"],"decision":"approve","duration":"forever","owner_uid":uid});
    if uid != 0 {
        assert!(super::super::permissions::decide(decision.clone(), &owner).is_err());
    }
    let mut foreign = decision.clone();
    foreign["owner_uid"] = json!(uid + 1);
    assert!(super::super::permissions::decide(foreign, &client(0)).is_err());
    let approved = super::super::permissions::decide(decision, &client(0)).unwrap();
    assert_eq!(approved["restoration"], "until_revoked");
    assert!(approved["expires_at"].is_null());
    assert!(approved["uses_remaining"].is_null());
    let journal = super::super::system_journal::query(json!({"source":"permission.decision"})).unwrap();
    let record = &journal["operations"][0];
    assert!(record["grant"].is_null());
    assert_eq!(record["restoration"]["until_revoked"], true);
    assert!(record["restoration"]["reference"].is_string());
    assert!(record["restoration"]["generation"].is_number());
    assert_eq!(
        control(query.clone(), &owner, None).await.unwrap()["permissions"][0]["enabled"],
        true
    );
    assert!(app_policy::require(
        uid + 1,
        "permission-fixture",
        &crate::caps::CapSet::from_caps([Cap::new(Verb::SYS_OBSERVE, Scope::name("audio"))])
    )
    .is_ok());
    control(revoke.clone(), &owner, None).await.unwrap();
    assert_eq!(
        control(query.clone(), &owner, None).await.unwrap()["permissions"][0]["enabled"],
        false
    );
    let restore = control(
        json!({"action":"request","app_id":"permission-fixture","permission_id":permission,"reason":"restore through OS review"}),
        &owner, None,
    ).await.unwrap();
    let displayed = super::super::system_review::show(
        json!({"id":restore["id"]}), &owner,
    ).await.unwrap();
    let displayed: clawd_client::system_review::SystemReview =
        serde_json::from_value(displayed).unwrap();
    use clawd_client::system_review::{PermissionChoice, PermissionSelection, ReviewAction, ReviewDecision};
    assert_eq!(displayed.subject.app_id.as_deref(), Some("permission-fixture"));
    assert_eq!(displayed.permissions[0].uses[0].purpose, "Audio status");
    assert_eq!(displayed.permissions[0].supported_choices,
        vec![PermissionChoice::Deny, PermissionChoice::Restore]);
    let typed = ReviewDecision {
        id: displayed.id.clone(), revision: displayed.revision, action: ReviewAction::ApplyChoices,
        choices: vec![PermissionSelection {
            permission_id: displayed.permissions[0].id.clone(), choice: PermissionChoice::Restore,
        }],
    };
    let restored = super::super::system_review::decide(
        json!({"owner_uid":uid,"review":typed}), &client(0),
    ).await.unwrap();
    assert_eq!(restored["status"], "completed");
    assert!(restored["permissions"][0]["current"].is_null());
    let receipt = approvals::list_recent_for_owner(100, Some(uid)).into_iter()
        .find(|resolved| resolved.request.id == displayed.id).unwrap();
    assert!(receipt.decision.grant.is_none());
    assert!(receipt.decision.restoration.is_some());
    assert_eq!(
        control(query.clone(), &owner, None).await.unwrap()["permissions"][0]["enabled"],
        true
    );
    control(revoke.clone(), &owner, None).await.unwrap();
    let mut forged = query.clone();
    forged["action"] = json!("revoke");
    forged["permission_id"] = json!("undeclared-expansion");
    assert!(control(forged, &owner, None).await.is_err());
    assert!(control(
        json!({"action":"show","app_id":"missing-app"}),
        &owner,
        None
    )
    .await
    .is_err());
    assert!(control(json!({"action":"approve"}), &owner, None)
        .await
        .is_err());
    let audit = crate::paths::data_dir().join("clawd/audit.jsonl");
    std::fs::remove_file(&audit).unwrap();
    std::fs::create_dir(&audit).unwrap();
    let error = control(revoke, &owner, None).await.unwrap_err();
    assert!(error.contains("was disabled") && error.contains("revocation audit failed"), "{error}");
    assert_eq!(
        control(query.clone(), &owner, None).await.unwrap()["permissions"][0]["enabled"],
        false
    );
    std::fs::write(app.join("app.json"), "{}").unwrap();
    assert!(control(query, &owner, None).await.is_err());
}

#[test]
fn wire_rejects_owner_and_confirmation_forgery() {
    use crate::clawd::routes::Command;
    for route in [Command::PermissionApps, Command::SystemAppPermissions] {
        for field in ["owner_uid", "confirm", "approve", "target_owner"] {
            let mut params = json!({"action":"request","app_id":"audio-manager","permission_id":"key","reason":"test"});
            params[field] = json!(true);
            assert!((route.route().decode)(params).is_err(), "{field}");
        }
    }
}
