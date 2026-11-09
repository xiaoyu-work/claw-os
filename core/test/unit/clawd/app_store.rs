use super::*;

#[test]
fn installation_tokens_cannot_select_filesystem_paths() {
    assert!(request_path("../other-owner").is_err());
    assert!(request_path("/etc/passwd").is_err());
    assert!(source_path("a".repeat(33).as_str()).is_err());
    let id = "a".repeat(32);
    assert_eq!(
        source_path(&id).unwrap(),
        root().join("staging").join(&id).join("package")
    );
}

#[tokio::test]
async fn store_mutation_refuses_same_app_admission_but_not_other_apps() {
    let app = format!("store-fence-{}", uuid::Uuid::new_v4().simple());
    let (started, running) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let target = app.clone();
    let controller = tokio::spawn(async move {
        let publishing_target = target.clone();
        with_mutation(&target, |_fence| async move {
            started.send(()).unwrap();
            released.await.unwrap();
            Ok(())
        }, |fence| async move {
            assert!(super::super::app_mutation::admit(&publishing_target).is_err());
            drop(fence);
            Ok(())
        }).await
    });
    running.await.unwrap();
    assert!(super::super::app_mutation::admit(&app).is_err(), "launch/bind/start must refuse the mutating App");
    assert!(super::super::app_mutation::admit("store-unrelated-app").is_ok());
    release.send(()).unwrap();
    controller.await.unwrap().unwrap();
    assert!(super::super::app_mutation::admit(&app).is_ok());
}

#[tokio::test]
async fn retirement_failure_cannot_publish_and_a_cleanup_retry_can() {
    let app = format!("store-retry-{}", uuid::Uuid::new_v4().simple());
    let published = std::sync::atomic::AtomicBool::new(false);
    let result = with_mutation(&app, |_fence| async { Err("provider still draining".into()) },
        |_fence| async {
            published.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        }).await;
    assert!(result.unwrap_err().contains("draining"));
    assert!(!published.load(std::sync::atomic::Ordering::Acquire));
    assert!(super::super::app_mutation::admit(&app).is_ok());
    with_mutation(&app, |_fence| async { Ok(()) }, |_fence| async {
        published.store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }).await.unwrap();
    assert!(published.load(std::sync::atomic::Ordering::Acquire));
}

#[tokio::test]
async fn cancelled_wait_cannot_release_a_cleanup_workers_mutation_fence() {
    let app = format!("store-cancel-{}", uuid::Uuid::new_v4().simple());
    let (started, running) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let (finished, complete) = tokio::sync::oneshot::channel();
    let target = app.clone();
    let controller: tokio::task::JoinHandle<Result<(), String>> = tokio::spawn(async move {
        with_mutation(&target, |fence| async move {
            tokio::spawn(async move {
                started.send(()).unwrap();
                released.await.unwrap();
                drop(fence);
                finished.send(()).unwrap();
                Ok::<_, String>(())
            }).await.unwrap()
        }, |_fence| async { panic!("a cancelled controller must not publish") }).await
    });
    running.await.unwrap();
    controller.abort();
    assert!(controller.await.unwrap_err().is_cancelled());
    assert!(super::super::app_mutation::admit(&app).is_err());
    release.send(()).unwrap();
    complete.await.unwrap();
    assert!(super::super::app_mutation::admit(&app).is_ok());
}

#[test]
fn store_refuses_dpkg_owned_subtrees_and_unknown_delivery_ownership() {
    let error = delivery_owner_result(Some(0),
        b"cos-app-example: /usr/lib/cos/apps/example/web/app.js\n").unwrap_err();
    assert!(error.contains("APT"));
    assert!(delivery_owner_result(Some(1), b"").is_ok());
    assert!(delivery_owner_result(Some(2), b"").is_err());
    assert!(delivery_owner_result(None, b"").is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn delivery_ownership_checks_canonical_directory_aliases() {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let installed = root.path().join("installed");
    let alias = root.path().join("alias");
    std::fs::create_dir(&installed).unwrap();
    std::os::unix::fs::symlink(&installed, &alias).unwrap();
    let paths = delivery_paths(&alias.join("example")).unwrap();
    assert!(paths.contains(&installed.join("example").to_str().unwrap().to_string()));
    assert!(paths.contains(&format!("{}/*", installed.join("example").display())));
}
