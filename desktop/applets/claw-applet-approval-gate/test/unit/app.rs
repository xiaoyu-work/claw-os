use super::{RefreshState, run_refresh};

#[test]
fn refresh_state_allows_only_one_in_flight_request() {
    let state = RefreshState::default();
    let first = state.try_start().expect("first refresh should start");

    assert!(state.is_in_flight());
    assert!(
        state.try_start().is_none(),
        "a second refresh must not start while the first is in flight"
    );

    drop(first);
    assert!(!state.is_in_flight());
    assert!(
        state.try_start().is_some(),
        "polling should resume after the first refresh finishes"
    );
}

#[tokio::test]
async fn refresh_state_resets_after_success_and_error() {
    let state = RefreshState::default();
    let success = run_refresh(
        state.try_start().expect("start successful refresh"),
        async { Ok::<(), &'static str>(()) },
    )
    .await;

    assert_eq!(success, Ok(()));
    assert!(!state.is_in_flight());

    let failure = run_refresh(state.try_start().expect("start failing refresh"), async {
        Err::<(), &'static str>("clawd unavailable")
    })
    .await;

    assert_eq!(failure, Err("clawd unavailable"));
    assert!(!state.is_in_flight());
}

#[tokio::test]
async fn refresh_state_resets_when_task_is_cancelled() {
    let state = RefreshState::default();
    let permit = state.try_start().expect("start cancellable refresh");
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(run_refresh(permit, async move {
        started_tx.send(()).expect("signal task start");
        std::future::pending::<()>().await;
    }));

    started_rx.await.expect("refresh task should start");
    assert!(state.is_in_flight());

    task.abort();
    assert!(
        task.await
            .expect_err("task should be cancelled")
            .is_cancelled()
    );
    assert!(!state.is_in_flight());
    assert!(
        state.try_start().is_some(),
        "polling should resume after cancellation"
    );
}
