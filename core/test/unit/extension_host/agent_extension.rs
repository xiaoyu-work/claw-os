use super::*;

#[test]
fn instance_nonces_are_high_entropy_shape_and_distinct() {
    let first = random_nonce().unwrap();
    let second = random_nonce().unwrap();
    assert_eq!(first.len(), 64);
    assert_eq!(second.len(), 64);
    assert_ne!(first, second);
    assert!(first
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
}
