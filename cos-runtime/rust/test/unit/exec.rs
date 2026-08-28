use super::*;

#[test]
fn start_stdin_is_bounded_before_invoking_cos() {
    let input = vec![0_u8; MAX_START_STDIN_BYTES + 1];
    assert!(matches!(
        start_with_stdin(&["program"], &input),
        Err(StartError::InputTooLarge {
            actual,
            limit: MAX_START_STDIN_BYTES
        }) if actual == MAX_START_STDIN_BYTES + 1
    ));
}
