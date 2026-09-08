use super::*;

#[test]
fn revocation_survives_process_restart() {
    let cap = Cap::new(Verb::SYS_OBSERVE, crate::caps::Scope::name("audio"));
    if std::env::var_os("CLAW_PERMISSION_RESTART_FIXTURE").is_some() {
        assert_eq!(blocks(1000, "audio-manager").unwrap()[0].generation, 1);
        assert!(require(1000, "audio-manager", &CapSet::from_caps([cap])).is_err());
        return;
    }
    let _lock = crate::test_env::lock_env();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../build");
    let dir = tempfile::tempdir_in(root).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", dir.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", dir.path());
    revoke(1000, "audio-manager", cap).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "approvals::app_policy::tests::revocation_survives_process_restart",
            "--test-threads=1",
        ])
        .env("CLAW_PERMISSION_RESTART_FIXTURE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn owner_app_revocation_persists_and_restoration_needs_trusted_decision() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../build/permission-policy-test");
    std::fs::create_dir_all(&root).unwrap();
    let dir = tempfile::tempdir_in(root).unwrap();
    let previous = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_DATA_DIR", dir.path());
    let cap = Cap::new(Verb::SYS_OBSERVE, crate::caps::Scope::name("audio"));
    let caps = CapSet::from_caps([cap.clone()]);
    assert!(require(1000, "audio-manager", &caps).is_ok());
    let block = revoke(1000, "audio-manager", cap.clone()).unwrap();
    assert!(require(1000, "audio-manager", &caps).is_err());
    assert!(require(1001, "audio-manager", &caps).is_ok());
    assert!(require(1000, "camera-manager", &caps).is_ok());
    assert_eq!(blocks(1000, "audio-manager").unwrap()[0].generation, 1);
    let id = super::super::submit_owned(
        cap.verb,
        cap.scope.clone(),
        block.session(1000, "audio-manager"),
        "restore",
        None,
        Some(1000),
    )
    .unwrap();
    assert!(require(1000, "audio-manager", &caps).is_err());
    assert!(super::super::approve_for_owner(
        &id,
        super::super::GrantDuration::Forever,
        None,
        None,
        Some(1001)
    )
    .is_err());
    assert!(super::super::approve_for_owner(
        &id,
        super::super::GrantDuration::Once,
        None,
        None,
        Some(1000)
    )
    .is_err());
    super::super::approve_for_owner(
        &id,
        super::super::GrantDuration::Forever,
        None,
        None,
        Some(1000),
    )
    .unwrap();
    assert!(require(1000, "audio-manager", &caps).is_ok());
    let next = revoke(1000, "audio-manager", cap).unwrap();
    assert_eq!(next.generation, 2);
    assert!(require(1000, "audio-manager", &caps).is_err());
    assert!(revoke(1000, "audio-manager", Cap::unscoped(Verb::FS_WRITE)).is_err());
    match previous {
        Some(value) => std::env::set_var("COS_DATA_DIR", value),
        None => std::env::remove_var("COS_DATA_DIR"),
    }
}
