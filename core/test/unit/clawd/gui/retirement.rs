use super::*;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

#[test]
fn system_approval_bucket_matches_neither_uid_zero_nor_other_owners() {
    for scope in [
        RevocationScope::Owner { uid: None },
        RevocationScope::Session {
            uid: None,
            session: "same-session".into(),
        },
    ] {
        for owner in [0, 62050] {
            assert!(!approval_matches(&scope, owner, |_| {
                panic!("another owner must not inspect session membership")
            }));
        }
    }
    assert!(approval_matches(
        &RevocationScope::Owner { uid: Some(0) },
        0,
        |_| panic!("owner scope does not inspect sessions"),
    ));
    assert!(!approval_matches(
        &RevocationScope::Owner { uid: Some(0) },
        62050,
        |_| true,
    ));
}

#[test]
fn approval_session_retirement_requires_owner_and_actual_membership() {
    let scope = RevocationScope::Session {
        uid: Some(62050),
        session: "parent".into(),
    };
    assert!(!approval_matches(&scope, 0, |_| true));
    assert!(!approval_matches(&scope, 62050, |_| false));
    assert!(approval_matches(&scope, 62050, |session| session == "parent"));
    let own = RevocationScope::Session {
        uid: Some(62050),
        session: "gui".into(),
    };
    assert!(approval_matches(&own, 62050, |session| {
        ["gui", "parent"].contains(&session)
    }));
}

#[tokio::test]
async fn completion_preserves_the_deadline_and_retirement_failure() {
    let deadline = Instant::now() + Duration::from_secs(1);
    wait(deadline, move |received| {
        assert_eq!(received, deadline);
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        wait(deadline, |_| Err("retained client remains".into()))
            .await
            .unwrap_err(),
        "retained client remains",
    );
}

#[test]
fn blocking_queue_time_counts_and_timeout_does_not_cancel_cleanup() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let (entered, started) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let blocker = runtime.spawn_blocking(move || {
        entered.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(3)).unwrap();
    });
    started.recv_timeout(Duration::from_secs(1)).unwrap();
    let received = Arc::new(Mutex::new(None));
    let recorded = received.clone();
    let deadline = Instant::now() + Duration::from_millis(30);
    let error = runtime
        .block_on(wait(deadline, move |actual| {
            *recorded.lock().unwrap() = Some(actual);
            Ok(())
        }))
        .unwrap_err();
    assert!(error.contains("deadline exceeded") && error.contains("pending"));
    assert!(Instant::now() < deadline + Duration::from_secs(1));
    assert!(received.lock().unwrap().is_none());
    release.send(()).unwrap();
    runtime.block_on(blocker).unwrap();
    runtime.shutdown_timeout(Duration::from_secs(1));
    assert_eq!(*received.lock().unwrap(), Some(deadline));
}
