use super::*;

fn client(uid: u32) -> ClientIdentity {
    ClientIdentity {
        uid: Some(uid),
        gid: Some(unsafe { libc::getegid() } as u32),
        ..ClientIdentity::unknown()
    }
}

#[tokio::test]
async fn account_logout_requires_confirmation_and_refuses_root() {
    assert_eq!(
        logout(json!({ "confirm": false }), &client(1000))
            .await
            .unwrap_err(),
        "Agent account logout requires confirm=true"
    );
    assert_eq!(
        status(json!({}), &client(0)).await.unwrap_err(),
        ROOT_OWNER_REFUSAL
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn account_status_and_logout_use_the_owner_credential_partition() {
    let owner_uid = unsafe { libc::geteuid() } as u32;
    if owner_uid == 0 {
        return;
    }
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let credentials = root.path().join("credentials");
    let credential_dir = credentials.join(NAMESPACE);
    std::fs::create_dir_all(&credential_dir).unwrap();
    let credential_path = credential_dir.join(format!("{}.json", credential_name()));
    std::fs::write(&credential_path, "{}").unwrap();
    let _credentials = crate::test_env::TestEnvVarGuard::set("COS_CREDENTIALS_DIR", &credentials);
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let client = client(owner_uid);

    assert_eq!(
        status(json!({}), &client).await.unwrap()["credential_present"],
        true
    );
    let result = logout(json!({ "confirm": true }), &client).await.unwrap();
    assert_eq!(result["was_present"], true);
    assert_eq!(result["credential_present"], false);
    assert!(!credential_path.exists());
}
