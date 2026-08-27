use super::*;

struct FirstPage;
struct SecondPage;

fn first_page() -> TypeId {
    TypeId::of::<FirstPage>()
}

fn second_page() -> TypeId {
    TypeId::of::<SecondPage>()
}

#[test]
fn marker_waits_for_every_page_success() {
    let mut coordinator = Coordinator::new(false);

    assert_eq!(
        coordinator.begin([first_page(), second_page()]),
        Some(Start::Apply { attempt: 1 })
    );
    assert!(coordinator.finishing());
    assert_eq!(
        coordinator.page_finished(1, first_page(), true),
        PageResult::Waiting
    );
    assert!(!coordinator.marker_ready);
    assert_eq!(
        coordinator.page_finished(1, second_page(), true),
        PageResult::WriteMarker
    );
    assert!(coordinator.marker_ready);
}

#[test]
fn page_failure_clears_finishing_and_allows_retry() {
    let mut coordinator = Coordinator::new(false);
    assert_eq!(
        coordinator.begin([first_page(), second_page()]),
        Some(Start::Apply { attempt: 1 })
    );

    assert_eq!(
        coordinator.page_finished(1, first_page(), false),
        PageResult::Failed
    );
    assert!(!coordinator.finishing());
    assert!(!coordinator.marker_ready);
    assert_eq!(
        coordinator.page_finished(1, second_page(), true),
        PageResult::Ignored
    );

    assert_eq!(
        coordinator.begin([first_page(), second_page()]),
        Some(Start::Apply { attempt: 2 })
    );
    assert_eq!(
        coordinator.page_finished(2, first_page(), true),
        PageResult::Waiting
    );
    assert_eq!(
        coordinator.page_finished(2, second_page(), true),
        PageResult::WriteMarker
    );
}

#[test]
fn duplicate_page_result_cannot_write_marker_twice() {
    let mut coordinator = Coordinator::new(false);
    assert_eq!(
        coordinator.begin([first_page()]),
        Some(Start::Apply { attempt: 1 })
    );
    assert_eq!(
        coordinator.page_finished(1, first_page(), true),
        PageResult::WriteMarker
    );
    assert_eq!(
        coordinator.page_finished(1, first_page(), true),
        PageResult::Ignored
    );
    assert_eq!(coordinator.begin([first_page()]), None);
}

#[test]
fn marker_failure_retries_without_reapplying_pages() {
    let mut coordinator = Coordinator::new(false);
    assert_eq!(
        coordinator.begin([first_page()]),
        Some(Start::Apply { attempt: 1 })
    );
    assert_eq!(
        coordinator.page_finished(1, first_page(), true),
        PageResult::WriteMarker
    );
    assert!(coordinator.operation_failed(1));
    assert!(!coordinator.finishing());

    assert_eq!(
        coordinator.begin([first_page()]),
        Some(Start::WriteMarker { attempt: 2 })
    );
    assert!(coordinator.finishing());
}
