use super::*;

#[test]
fn refuses_last_output_disable_in_validation_inputs() {
    validate_action(
        "disable",
        Some("eDP-1"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        false,
    )
    .unwrap();
}
