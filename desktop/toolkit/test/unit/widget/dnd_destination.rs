use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
enum TestMsg {
    Data,
    Finished,
}

#[test]
fn data_before_drop_invokes_data_handler_only() {
    let mut state: State<()> = State::new();
    assert!(state.drag_offer.is_none());
    state.on_enter::<TestMsg>(
        4.0,
        2.0,
        vec!["text/plain".into()],
        Option::<fn(_, _, _) -> TestMsg>::None,
        (),
    );
    let (message, status) = state.on_data_received(
        "text/plain".into(),
        vec![1],
        Some(|mime, data| {
            assert_eq!(mime, "text/plain");
            assert_eq!(data, vec![1]);
            TestMsg::Data
        }),
        Option::<fn(_, _, _, _, _) -> TestMsg>::None,
    );
    assert!(matches!(message, Some(TestMsg::Data)));
    assert_eq!(status, event::Status::Captured);
    assert!(state.drag_offer.is_some());
}

#[test]
fn finish_only_emits_after_drop() {
    let mut state: State<()> = State::new();
    state.on_enter::<TestMsg>(
        5.0,
        -1.0,
        vec![],
        Option::<fn(_, _, _) -> TestMsg>::None,
        (),
    );
    state.on_action_selected::<TestMsg>(DndAction::Move, Option::<fn(_) -> TestMsg>::None);
    state.on_drop::<TestMsg>(Option::<fn(_, _) -> TestMsg>::None);

    let (message, status) = state.on_data_received(
        "application/x-test".into(),
        vec![7],
        Option::<fn(_, _) -> TestMsg>::None,
        Some(|mime, data, action, x, y| {
            assert_eq!(mime, "application/x-test");
            assert_eq!(data, vec![7]);
            assert_eq!(action, DndAction::Move);
            assert_eq!(x, 5.0);
            assert_eq!(y, -1.0);
            TestMsg::Finished
        }),
    );
    assert!(matches!(message, Some(TestMsg::Finished)));
    assert_eq!(status, event::Status::Captured);
    assert!(state.drag_offer.is_none());
}
