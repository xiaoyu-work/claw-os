use super::*;

fn service(
    lifecycle: crate::caps::manifest::McpLifecycle,
    running: bool,
    last_used: Instant,
) -> AppServiceSnapshot {
    AppServiceSnapshot {
        app_id: "email".to_string(),
        lifecycle,
        tool: "email.search".to_string(),
        last_used,
        running,
        retry_at: None,
        call_lock: Arc::new(Mutex::new(())),
    }
}

#[test]
fn manifest_lifecycle_has_one_deterministic_run_policy() {
    let now = Instant::now();
    assert!(service_should_run(
        &service(crate::caps::manifest::McpLifecycle::AlwaysOn, false, now),
        false,
    ));
    assert!(service_should_run(
        &service(
            crate::caps::manifest::McpLifecycle::WhileAppRunning,
            false,
            now,
        ),
        true,
    ));
    assert!(!service_should_run(
        &service(
            crate::caps::manifest::McpLifecycle::WhileAppRunning,
            true,
            now,
        ),
        false,
    ));
    assert!(service_should_run(
        &service(crate::caps::manifest::McpLifecycle::Lazy, true, now),
        false,
    ));
    assert!(!service_should_run(
        &service(
            crate::caps::manifest::McpLifecycle::Lazy,
            true,
            now.checked_sub(LAZY_APP_IDLE).unwrap(),
        ),
        false,
    ));
}

#[tokio::test]
async fn service_calls_share_one_lock_and_restart_backoff_is_bounded() {
    let slot = OwnerHostSlot {
        owner_uid: 1000,
        owner_home: PathBuf::from("/home/test"),
        runtime: Mutex::new(None),
        services: Mutex::new(HashMap::new()),
        restart: Mutex::new(RestartState::default()),
    };
    let first = track_service(
        &slot,
        "email",
        "email.search",
        crate::caps::manifest::McpLifecycle::Lazy,
    )
    .await;
    let second = track_service(
        &slot,
        "email",
        "email.search",
        crate::caps::manifest::McpLifecycle::AlwaysOn,
    )
    .await;
    assert!(Arc::ptr_eq(&first, &second));

    for _ in 0..10 {
        record_host_failure(&slot).await;
    }

    let restart = slot.restart.lock().await;
    assert_eq!(restart.failures, 10);
    assert!(
        restart
            .retry_at
            .unwrap()
            .saturating_duration_since(Instant::now())
            <= APP_RESTART_MAX
    );
}

#[tokio::test]
async fn two_tasks_for_one_owner_share_one_persistent_host_slot() {
    let runtime = tempfile::tempdir().unwrap();
    let manager = PersistentAppHostManager::new(
        DaemonState::try_new().unwrap(),
        Admission::new(crate::clawd::transport::limits::Limits::default()),
        runtime.path().join("clawd.sock"),
        unsafe { libc::getegid() as u32 },
    );
    let home = PathBuf::from("/home/owner");
    let first = manager.owner_slot(1000, &home).await.unwrap();
    let second = manager.owner_slot(1000, &home).await.unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(manager.hosts.lock().await.len(), 1);
    let replaced = PathBuf::from("/home/replaced");
    assert!(manager.owner_slot(1000, &replaced).await.is_err());
}
