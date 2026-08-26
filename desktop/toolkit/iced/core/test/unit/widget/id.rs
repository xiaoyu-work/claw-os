use super::Id;

#[test]
fn unique_generates_different_ids() {
    let a = Id::unique();
    let b = Id::unique();

    assert_ne!(a, b);
}
