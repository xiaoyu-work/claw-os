use super::*;

#[test]
fn same_session_conflicts_until_lease_drops() {
    let leases = TurnLeaseRegistry::default();
    let lease = leases.try_acquire("same-session").unwrap();

    assert_eq!(
        leases.try_acquire("same-session").err(),
        Some(TurnAlreadyActive)
    );
    assert_eq!(leases.active_count(), 1);

    drop(lease);
    assert!(leases.try_acquire("same-session").is_ok());
}

#[test]
fn unrelated_sessions_are_not_serialized() {
    let leases = TurnLeaseRegistry::default();
    let _first = leases.try_acquire("session-a").unwrap();
    let _second = leases.try_acquire("session-b").unwrap();

    assert_eq!(leases.active_count(), 2);
}

#[test]
fn error_path_releases_lease() {
    fn fail(leases: &TurnLeaseRegistry) -> Result<(), &'static str> {
        let _lease = leases.try_acquire("error-session").unwrap();
        Err("synthetic failure")
    }

    let leases = TurnLeaseRegistry::default();
    assert_eq!(fail(&leases), Err("synthetic failure"));
    assert!(leases.try_acquire("error-session").is_ok());
}

#[tokio::test]
async fn aborting_owner_task_releases_lease() {
    let leases = TurnLeaseRegistry::default();
    let lease = leases.try_acquire("cancelled-session").unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _lease = lease;
        let _ = started_tx.send(());
        std::future::pending::<()>().await;
    });

    started_rx.await.unwrap();
    assert_eq!(
        leases.try_acquire("cancelled-session").err(),
        Some(TurnAlreadyActive)
    );

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(leases.try_acquire("cancelled-session").is_ok());
}
