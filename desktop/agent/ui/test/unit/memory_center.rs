use super::*;

#[test]
fn memory_navigation_rejects_late_responses_and_builds_an_owner_view() {
    crate::localize::localize();
    let mut center = MemoryCenter::default();
    let _ = center.update(Message::Show, None);
    assert!(center.is_visible());
    let old = center.generation;
    center.hide();
    let _ = center.update(Message::Listed(old, Ok(MemoryInventory {
        entries: Vec::new(), has_more: false, coverage: "late source".into(),
    })), None);
    assert_ne!(center.inventory.coverage, "late source");
    let _ = center.view();
}
