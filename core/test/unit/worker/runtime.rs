use super::*;
use crate::test_env::TestEnvVarGuard;

#[tokio::test]
async fn routed_runtime_uses_owner_storage_not_the_broker_data_root() {
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::tempdir().unwrap();
    let broker = directory.path().join("broker");
    let owner = directory.path().join("owner");
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", &broker);
    let _user = TestEnvVarGuard::set("COS_USER_DATA_DIR", &owner);
    let _xdg = TestEnvVarGuard::set("XDG_RUNTIME_DIR", directory.path().join("absent"));
    assert_eq!(root(), broker.join("worker"));
    crate::paths::with_routed_job(crate::paths::with_user_override(
        1000,
        directory.path().join("home"),
        async {
            assert_eq!(root(), owner.join("worker"));
            let launch = LaunchDir::create("fixture", None).unwrap();
            assert!(launch.path().starts_with(&owner));
            assert!(!broker.exists());
        },
    ))
    .await;
}

#[tokio::test]
async fn user_runtime_directory_keeps_precedence_for_routed_launches() {
    let _lock = crate::test_env::lock_env();
    let directory = tempfile::tempdir().unwrap();
    let xdg = directory.path().join("runtime");
    std::fs::create_dir(&xdg).unwrap();
    let _xdg = TestEnvVarGuard::set("XDG_RUNTIME_DIR", &xdg);
    crate::paths::with_routed_job(async {
        assert_eq!(root(), xdg.join("cos-worker"));
    })
    .await;
}
