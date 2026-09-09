use super::*;

#[test]
fn numerical_ids_are_sender_bound_not_hints_or_labels() {
    let (tx, _) = channel(10);
    let mut daemon = Notifications::new(tx);
    let (first, replaced) = daemon.allocate(":1.1", 0).unwrap();
    assert!(!replaced);
    assert_eq!(daemon.allocate(":1.1", first).unwrap(), (first, true));
    assert!(daemon.allocate(":1.2", first).is_err());
    let (second, replaced) = daemon.allocate(":1.2", 999).unwrap();
    assert!(!replaced);
    assert_ne!(second, first);
    assert_ne!(second, 999, "unknown replacement ids cannot become aliases");
    daemon.next = NonZeroU32::new(u32::MAX).unwrap();
    daemon.allocate(":1.2", 0).unwrap();
    assert_ne!(daemon.allocate(":1.2", 0).unwrap().0, first);
}
